use crate::ty;
use crate::ty::{EarlyBinder, GenericArgsRef};
use rustc_ast as ast;
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def_id::DefId;
use rustc_span::symbol::{kw, Symbol};
use rustc_span::Span;

use super::{Clause, EarlyBoundRegion, InstantiatedPredicates, ParamConst, ParamTy, Ty, TyCtxt};

#[derive(Clone, Debug, TyEncodable, TyDecodable, HashStable)]
pub enum GenericParamDefKind {
    Lifetime,
    Type { has_default: bool, synthetic: bool },
    Const { has_default: bool },
}

impl GenericParamDefKind {
    pub fn descr(&self) -> &'static str {
        match self {
            GenericParamDefKind::Lifetime => "lifetime",
            GenericParamDefKind::Type { .. } => "type",
            GenericParamDefKind::Const { .. } => "constant",
        }
    }
    pub fn to_ord(&self) -> ast::ParamKindOrd {
        match self {
            GenericParamDefKind::Lifetime => ast::ParamKindOrd::Lifetime,
            GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
                ast::ParamKindOrd::TypeOrConst
            }
        }
    }

    pub fn is_ty_or_const(&self) -> bool {
        match self {
            GenericParamDefKind::Lifetime => false,
            GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => true,
        }
    }

    pub fn is_synthetic(&self) -> bool {
        match self {
            GenericParamDefKind::Type { synthetic, .. } => *synthetic,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, TyEncodable, TyDecodable, HashStable)]
pub struct GenericParamDef {
    pub name: Symbol,
    pub def_id: DefId,
    pub index: u32,

    /// `pure_wrt_drop`, set by the (unsafe) `#[may_dangle]` attribute
    /// on generic parameter `'a`/`T`, asserts data behind the parameter
    /// `'a`/`T` won't be accessed during the parent type's `Drop` impl.
    pub pure_wrt_drop: bool,

    pub kind: GenericParamDefKind,
}

impl GenericParamDef {
    pub fn to_early_bound_region_data(&self) -> ty::EarlyBoundRegion {
        if let GenericParamDefKind::Lifetime = self.kind {
            ty::EarlyBoundRegion { def_id: self.def_id, index: self.index, name: self.name }
        } else {
            bug!("cannot convert a non-lifetime parameter def to an early bound region")
        }
    }

    pub fn is_anonymous_lifetime(&self) -> bool {
        match self.kind {
            GenericParamDefKind::Lifetime => {
                self.name == kw::UnderscoreLifetime || self.name == kw::Empty
            }
            _ => false,
        }
    }

    pub fn default_value<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
    ) -> Option<EarlyBinder<ty::GenericArg<'tcx>>> {
        match self.kind {
            GenericParamDefKind::Type { has_default, .. } if has_default => {
                Some(tcx.type_of(self.def_id).map_bound(|t| t.into()))
            }
            GenericParamDefKind::Const { has_default } if has_default => {
                Some(tcx.const_param_default(self.def_id).map_bound(|c| c.into()))
            }
            _ => None,
        }
    }

    pub fn to_error<'tcx>(
        &self,
        tcx: TyCtxt<'tcx>,
        preceding_args: &[ty::GenericArg<'tcx>],
    ) -> ty::GenericArg<'tcx> {
        match &self.kind {
            ty::GenericParamDefKind::Lifetime => ty::Region::new_error_misc(tcx).into(),
            ty::GenericParamDefKind::Type { .. } => Ty::new_misc_error(tcx).into(),
            ty::GenericParamDefKind::Const { .. } => ty::Const::new_misc_error(
                tcx,
                tcx.type_of(self.def_id).instantiate(tcx, preceding_args),
            )
            .into(),
        }
    }
}

#[derive(Default)]
pub struct GenericParamCount {
    pub lifetimes: usize,
    pub types: usize,
    pub consts: usize,
}

/// Information about the formal type/lifetime parameters associated
/// with an item or method. Analogous to `hir::Generics`.
///
/// The ordering of parameters is the same as in `Subst` (excluding child generics):
/// `Self` (optionally), `Lifetime` params..., `Type` params...
#[derive(Clone, Debug, TyEncodable, TyDecodable, HashStable)]
pub struct Generics {
    pub parent: Option<DefId>,
    pub parent_count: usize,
    pub params: Vec<GenericParamDef>,

    /// Reverse map to the `index` field of each `GenericParamDef`.
    #[stable_hasher(ignore)]
    pub param_def_id_to_index: FxHashMap<DefId, u32>,

    pub has_self: bool,
    pub has_late_bound_regions: Option<Span>,

    // The index of the host effect when substituted. (i.e. might be index to parent args)
    pub host_effect_index: Option<usize>,
}

impl<'tcx> Generics {
    /// Looks through the generics and all parents to find the index of the
    /// given param def-id. This is in comparison to the `param_def_id_to_index`
    /// struct member, which only stores information about this item's own
    /// generics.
    pub fn param_def_id_to_index(&self, tcx: TyCtxt<'tcx>, def_id: DefId) -> Option<u32> {
        if let Some(idx) = self.param_def_id_to_index.get(&def_id) {
            Some(*idx)
        } else if let Some(parent) = self.parent {
            let parent = tcx.generics_of(parent);
            parent.param_def_id_to_index(tcx, def_id)
        } else {
            None
        }
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.parent_count + self.params.len()
    }

    pub fn own_counts(&self) -> GenericParamCount {
        // We could cache this as a property of `GenericParamCount`, but
        // the aim is to refactor this away entirely eventually and the
        // presence of this method will be a constant reminder.
        let mut own_counts = GenericParamCount::default();

        for param in &self.params {
            match param.kind {
                GenericParamDefKind::Lifetime => own_counts.lifetimes += 1,
                GenericParamDefKind::Type { .. } => own_counts.types += 1,
                GenericParamDefKind::Const { .. } => own_counts.consts += 1,
            }
        }

        own_counts
    }

    pub fn own_defaults(&self) -> GenericParamCount {
        let mut own_defaults = GenericParamCount::default();

        for param in &self.params {
            match param.kind {
                GenericParamDefKind::Lifetime => (),
                GenericParamDefKind::Type { has_default, .. } => {
                    own_defaults.types += has_default as usize;
                }
                GenericParamDefKind::Const { has_default } => {
                    own_defaults.consts += has_default as usize;
                }
            }
        }

        own_defaults
    }

    pub fn requires_monomorphization(&self, tcx: TyCtxt<'tcx>) -> bool {
        if self.own_requires_monomorphization() {
            return true;
        }

        if let Some(parent_def_id) = self.parent {
            let parent = tcx.generics_of(parent_def_id);
            parent.requires_monomorphization(tcx)
        } else {
            false
        }
    }

    pub fn own_requires_monomorphization(&self) -> bool {
        for param in &self.params {
            match param.kind {
                GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
                    return true;
                }
                GenericParamDefKind::Lifetime => {}
            }
        }
        false
    }

    /// Returns the `GenericParamDef` with the given index.
    pub fn param_at(&'tcx self, param_index: usize, tcx: TyCtxt<'tcx>) -> &'tcx GenericParamDef {
        if let Some(index) = param_index.checked_sub(self.parent_count) {
            &self.params[index]
        } else {
            tcx.generics_of(self.parent.expect("parent_count > 0 but no parent?"))
                .param_at(param_index, tcx)
        }
    }

    pub fn params_to(&'tcx self, param_index: usize, tcx: TyCtxt<'tcx>) -> &'tcx [GenericParamDef] {
        if let Some(index) = param_index.checked_sub(self.parent_count) {
            &self.params[..index]
        } else {
            tcx.generics_of(self.parent.expect("parent_count > 0 but no parent?"))
                .params_to(param_index, tcx)
        }
    }

    /// Returns the `GenericParamDef` associated with this `EarlyBoundRegion`.
    pub fn region_param(
        &'tcx self,
        param: &EarlyBoundRegion,
        tcx: TyCtxt<'tcx>,
    ) -> &'tcx GenericParamDef {
        let param = self.param_at(param.index as usize, tcx);
        match param.kind {
            GenericParamDefKind::Lifetime => param,
            _ => bug!("expected lifetime parameter, but found another generic parameter"),
        }
    }

    /// Returns the `GenericParamDef` associated with this `ParamTy`.
    pub fn type_param(&'tcx self, param: &ParamTy, tcx: TyCtxt<'tcx>) -> &'tcx GenericParamDef {
        let param = self.param_at(param.index as usize, tcx);
        match param.kind {
            GenericParamDefKind::Type { .. } => param,
            _ => bug!("expected type parameter, but found another generic parameter"),
        }
    }

    /// Returns the `GenericParamDef` associated with this `ParamConst`.
    pub fn const_param(&'tcx self, param: &ParamConst, tcx: TyCtxt<'tcx>) -> &GenericParamDef {
        let param = self.param_at(param.index as usize, tcx);
        match param.kind {
            GenericParamDefKind::Const { .. } => param,
            _ => bug!("expected const parameter, but found another generic parameter"),
        }
    }

    /// Returns `true` if `params` has `impl Trait`.
    pub fn has_impl_trait(&'tcx self) -> bool {
        self.params.iter().any(|param| {
            matches!(param.kind, ty::GenericParamDefKind::Type { synthetic: true, .. })
        })
    }

    /// Returns the args corresponding to the generic parameters
    /// of this item, excluding `Self`.
    ///
    /// **This should only be used for diagnostics purposes.**
    pub fn own_args_no_defaults(
        &'tcx self,
        tcx: TyCtxt<'tcx>,
        args: &'tcx [ty::GenericArg<'tcx>],
    ) -> &'tcx [ty::GenericArg<'tcx>] {
        let mut own_params = self.parent_count..self.count();
        if self.has_self && self.parent.is_none() {
            own_params.start = 1;
        }

        // Filter the default arguments.
        //
        // This currently uses structural equality instead
        // of semantic equivalence. While not ideal, that's
        // good enough for now as this should only be used
        // for diagnostics anyways.
        own_params.end -= self
            .params
            .iter()
            .rev()
            .take_while(|param| {
                param.default_value(tcx).is_some_and(|default| {
                    default.instantiate(tcx, args) == args[param.index as usize]
                })
            })
            .count();

        &args[own_params]
    }

    /// Returns the args corresponding to the generic parameters of this item, excluding `Self`.
    ///
    /// **This should only be used for diagnostics purposes.**
    pub fn own_args(
        &'tcx self,
        args: &'tcx [ty::GenericArg<'tcx>],
    ) -> &'tcx [ty::GenericArg<'tcx>] {
        let own = &args[self.parent_count..][..self.params.len()];
        if self.has_self && self.parent.is_none() { &own[1..] } else { &own }
    }
}

/// Bounds on generics.
#[derive(Copy, Clone, Default, Debug, TyEncodable, TyDecodable, HashStable)]
pub struct GenericPredicates<'tcx> {
    pub parent: Option<DefId>,
    pub predicates: &'tcx [(Clause<'tcx>, Span)],
}

impl<'tcx> GenericPredicates<'tcx> {
    pub fn instantiate(
        &self,
        tcx: TyCtxt<'tcx>,
        args: GenericArgsRef<'tcx>,
    ) -> InstantiatedPredicates<'tcx> {
        let mut instantiated = InstantiatedPredicates::empty();
        self.instantiate_into(tcx, &mut instantiated, args);
        instantiated
    }

    pub fn instantiate_own(
        &self,
        tcx: TyCtxt<'tcx>,
        args: GenericArgsRef<'tcx>,
    ) -> impl Iterator<Item = (Clause<'tcx>, Span)> + DoubleEndedIterator + ExactSizeIterator {
        EarlyBinder::bind(self.predicates).arg_iter_copied(tcx, args)
    }

    #[instrument(level = "debug", skip(self, tcx))]
    fn instantiate_into(
        &self,
        tcx: TyCtxt<'tcx>,
        instantiated: &mut InstantiatedPredicates<'tcx>,
        args: GenericArgsRef<'tcx>,
    ) {
        if let Some(def_id) = self.parent {
            tcx.predicates_of(def_id).instantiate_into(tcx, instantiated, args);
        }
        instantiated.predicates.extend(
            self.predicates.iter().map(|(p, _)| EarlyBinder::bind(*p).instantiate(tcx, args)),
        );
        instantiated.spans.extend(self.predicates.iter().map(|(_, sp)| *sp));
    }

    pub fn instantiate_identity(&self, tcx: TyCtxt<'tcx>) -> InstantiatedPredicates<'tcx> {
        let mut instantiated = InstantiatedPredicates::empty();
        self.instantiate_identity_into(tcx, &mut instantiated);
        instantiated
    }

    fn instantiate_identity_into(
        &self,
        tcx: TyCtxt<'tcx>,
        instantiated: &mut InstantiatedPredicates<'tcx>,
    ) {
        if let Some(def_id) = self.parent {
            tcx.predicates_of(def_id).instantiate_identity_into(tcx, instantiated);
        }
        instantiated.predicates.extend(self.predicates.iter().map(|(p, _)| p));
        instantiated.spans.extend(self.predicates.iter().map(|(_, s)| s));
    }
}
