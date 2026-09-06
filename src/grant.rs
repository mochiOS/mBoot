use crate::domain::DomainId;

pub const MAX_GRANTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrantRef(u32);

impl GrantRef {
    pub const fn new(value: u32) -> Option<Self> {
        let slot = value & 0xff;
        if slot == 0 || slot as usize > MAX_GRANTS || value >> 8 == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    const fn index(self) -> usize {
        (self.0 & 0xff) as usize - 1
    }

    const fn generation(self) -> u32 {
        self.0 >> 8
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantError {
    InvalidPage,
    InvalidTarget,
    TableFull,
    UnknownGrant,
    AccessDenied,
    AlreadyMapped,
    NotMapped,
    Busy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Grant {
    owner: DomainId,
    target: DomainId,
    host_page: u64,
    page_count: u32,
    writable: bool,
    target_page: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GrantSlot {
    generation: u32,
    grant: Option<Grant>,
}

impl GrantSlot {
    const EMPTY: Self = Self {
        generation: 0,
        grant: None,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrantMapping {
    pub owner: DomainId,
    pub target: DomainId,
    pub host_page: u64,
    pub target_page: u64,
    pub page_count: u32,
    pub writable: bool,
}

pub struct GrantTable {
    grants: [GrantSlot; MAX_GRANTS],
}

impl GrantTable {
    pub const fn new() -> Self {
        Self {
            grants: [GrantSlot::EMPTY; MAX_GRANTS],
        }
    }

    pub fn create(
        &mut self,
        owner: DomainId,
        target: DomainId,
        host_page: u64,
        writable: bool,
    ) -> Result<GrantRef, GrantError> {
        self.create_range(owner, target, host_page, 1, writable)
    }

    pub fn create_range(
        &mut self,
        owner: DomainId,
        target: DomainId,
        host_page: u64,
        page_count: u32,
        writable: bool,
    ) -> Result<GrantRef, GrantError> {
        if owner == target {
            return Err(GrantError::InvalidTarget);
        }
        if host_page == 0
            || host_page & 0xfff != 0
            || page_count == 0
            || u64::from(page_count)
                .checked_mul(4096)
                .and_then(|length| host_page.checked_add(length))
                .is_none()
        {
            return Err(GrantError::InvalidPage);
        }
        let Some(index) = self.grants.iter().position(|slot| slot.grant.is_none()) else {
            return Err(GrantError::TableFull);
        };
        let generation = self.grants[index].generation.wrapping_add(1) & 0x00ff_ffff;
        self.grants[index].generation = generation.max(1);
        self.grants[index].grant = Some(Grant {
            owner,
            target,
            host_page,
            page_count,
            writable,
            target_page: None,
        });
        Ok(GrantRef(
            (self.grants[index].generation << 8) | (index + 1) as u32,
        ))
    }

    pub fn map(
        &mut self,
        target: DomainId,
        reference: GrantRef,
        target_page: u64,
    ) -> Result<GrantMapping, GrantError> {
        if target_page & 0xfff != 0 {
            return Err(GrantError::InvalidPage);
        }
        let grant = *self.grant(reference)?;
        if grant.target != target {
            return Err(GrantError::AccessDenied);
        }
        if grant.target_page.is_some() {
            return Err(GrantError::AlreadyMapped);
        }
        if self.target_range_is_mapped(target, target_page, grant.page_count) {
            return Err(GrantError::AlreadyMapped);
        }
        let grant = self.grant_mut(reference)?;
        grant.target_page = Some(target_page);
        Ok(GrantMapping {
            owner: grant.owner,
            target,
            host_page: grant.host_page,
            target_page,
            page_count: grant.page_count,
            writable: grant.writable,
        })
    }

    pub fn unmap(
        &mut self,
        target: DomainId,
        reference: GrantRef,
    ) -> Result<GrantMapping, GrantError> {
        let grant = self.grant_mut(reference)?;
        if grant.target != target {
            return Err(GrantError::AccessDenied);
        }
        let target_page = grant.target_page.take().ok_or(GrantError::NotMapped)?;
        Ok(GrantMapping {
            owner: grant.owner,
            target,
            host_page: grant.host_page,
            target_page,
            page_count: grant.page_count,
            writable: grant.writable,
        })
    }

    pub fn revoke(&mut self, owner: DomainId, reference: GrantRef) -> Result<(), GrantError> {
        let grant = self.grant(reference)?;
        if grant.owner != owner {
            return Err(GrantError::AccessDenied);
        }
        if grant.target_page.is_some() {
            return Err(GrantError::Busy);
        }
        self.grants[reference.index()].grant = None;
        Ok(())
    }

    pub fn references_domain(&self, domain: DomainId) -> bool {
        self.grants
            .iter()
            .filter_map(|slot| slot.grant.as_ref())
            .any(|grant| grant.owner == domain || grant.target == domain)
    }

    pub fn query_target(&self, target: DomainId, ordinal: usize) -> Option<GrantRef> {
        self.grants
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.grant.is_some_and(|grant| grant.target == target))
            .nth(ordinal)
            .map(|(index, slot)| GrantRef((slot.generation << 8) | (index + 1) as u32))
    }

    pub fn mapping_page_count(&self, target: DomainId, reference: GrantRef) -> Option<u32> {
        let grant = self.grant(reference).ok()?;
        (grant.target == target).then_some(grant.page_count)
    }

    pub fn cleanup_domain(&mut self, domain: DomainId) -> [Option<GrantMapping>; MAX_GRANTS] {
        self.cleanup_domain_inner(domain, false)
    }

    pub fn cleanup_crashed_domain(
        &mut self,
        domain: DomainId,
    ) -> [Option<GrantMapping>; MAX_GRANTS] {
        self.cleanup_domain_inner(domain, true)
    }

    fn cleanup_domain_inner(
        &mut self,
        domain: DomainId,
        revoke_unmapped_target: bool,
    ) -> [Option<GrantMapping>; MAX_GRANTS] {
        let mut mappings = [None; MAX_GRANTS];
        let mut mapping_count = 0;
        for slot in &mut self.grants {
            let Some(grant) = slot.grant else {
                continue;
            };
            let owner_is_stopping = grant.owner == domain;
            let target_is_stopping =
                grant.target == domain && (revoke_unmapped_target || grant.target_page.is_some());
            if !owner_is_stopping && !target_is_stopping {
                continue;
            }
            if let Some(target_page) = grant.target_page {
                mappings[mapping_count] = Some(GrantMapping {
                    owner: grant.owner,
                    target: grant.target,
                    host_page: grant.host_page,
                    target_page,
                    page_count: grant.page_count,
                    writable: grant.writable,
                });
                mapping_count += 1;
            }
            slot.grant = None;
        }
        mappings
    }

    pub fn target_page_is_mapped(&self, target: DomainId, target_page: u64) -> bool {
        self.target_range_is_mapped(target, target_page, 1)
    }

    pub fn target_range_is_mapped(
        &self,
        target: DomainId,
        target_page: u64,
        page_count: u32,
    ) -> bool {
        let Some(length) = u64::from(page_count).checked_mul(4096) else {
            return true;
        };
        let Some(end) = target_page.checked_add(length) else {
            return true;
        };
        self.grants
            .iter()
            .filter_map(|slot| slot.grant.as_ref())
            .filter(|grant| grant.target == target)
            .filter_map(|grant| {
                let start = grant.target_page?;
                let length = u64::from(grant.page_count).checked_mul(4096)?;
                Some((start, start.checked_add(length)?))
            })
            .any(|(start, mapped_end)| target_page < mapped_end && start < end)
    }

    fn grant(&self, reference: GrantRef) -> Result<&Grant, GrantError> {
        let slot = &self.grants[reference.index()];
        if slot.generation != reference.generation() {
            return Err(GrantError::UnknownGrant);
        }
        slot.grant.as_ref().ok_or(GrantError::UnknownGrant)
    }

    fn grant_mut(&mut self, reference: GrantRef) -> Result<&mut Grant, GrantError> {
        let slot = &mut self.grants[reference.index()];
        if slot.generation != reference.generation() {
            return Err(GrantError::UnknownGrant);
        }
        slot.grant.as_mut().ok_or(GrantError::UnknownGrant)
    }
}

impl Default for GrantTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_lifecycle_enforces_endpoint_ownership() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table.create(owner, target, 0x8000, true).unwrap();
        assert_eq!(
            table.map(DomainId::new(3), reference, 0x9000),
            Err(GrantError::AccessDenied)
        );
        let mapping = table.map(target, reference, 0x9000).unwrap();
        assert_eq!(mapping.host_page, 0x8000);
        assert!(mapping.writable);
        assert_eq!(table.revoke(owner, reference), Err(GrantError::Busy));
        assert_eq!(table.unmap(target, reference), Ok(mapping));
        assert_eq!(table.revoke(owner, reference), Ok(()));
    }

    #[test]
    fn range_grant_maps_and_detects_overlapping_targets() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table
            .create_range(owner, target, 0x8000, 3, true)
            .unwrap();
        let mapping = table.map(target, reference, 0x20_000).unwrap();
        assert_eq!(mapping.page_count, 3);
        assert!(table.target_page_is_mapped(target, 0x21_000));
        let overlap = table
            .create_range(owner, target, 0x40_000, 2, false)
            .unwrap();
        assert_eq!(
            table.map(target, overlap, 0x22_000),
            Err(GrantError::AlreadyMapped)
        );
        assert_eq!(table.unmap(target, reference), Ok(mapping));
    }

    #[test]
    fn grant_refs_are_nonzero_and_stale_refs_are_rejected() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table.create(owner, target, 0x8000, false).unwrap();
        assert_ne!(reference.get(), 0);
        assert_eq!(table.revoke(owner, reference), Ok(()));
        let replacement = table.create(owner, target, 0xa000, false).unwrap();
        assert_ne!(replacement, reference);
        assert_eq!(
            table.map(target, reference, 0x9000),
            Err(GrantError::UnknownGrant)
        );
    }

    #[test]
    fn target_discovers_only_its_own_grants() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let first = table.create(owner, target, 0x8000, true).unwrap();
        let other = table.create(owner, DomainId::new(3), 0x9000, true).unwrap();
        let second = table.create(owner, target, 0xa000, false).unwrap();
        assert_eq!(table.query_target(target, 0), Some(first));
        assert_eq!(table.query_target(target, 1), Some(second));
        assert_eq!(table.query_target(target, 2), None);
        assert_eq!(table.query_target(DomainId::new(3), 0), Some(other));
    }

    #[test]
    fn cleanup_returns_live_mappings_and_revokes_refs() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table.create(owner, target, 0x8000, true).unwrap();
        let mapping = table.map(target, reference, 0x9000).unwrap();
        let cleanup = table.cleanup_domain(owner);
        assert_eq!(cleanup[0], Some(mapping));
        assert!(!table.references_domain(owner));
        assert_eq!(
            table.unmap(target, reference),
            Err(GrantError::UnknownGrant)
        );
    }

    #[test]
    fn stopped_unmapped_target_does_not_steal_owner_revoke() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table.create(owner, target, 0x8000, true).unwrap();
        assert_eq!(table.cleanup_domain(target), [None; MAX_GRANTS]);
        assert_eq!(table.revoke(owner, reference), Ok(()));
    }

    #[test]
    fn crashed_unmapped_target_revokes_the_stale_grant() {
        let mut table = GrantTable::new();
        let owner = DomainId::new(1);
        let target = DomainId::new(2);
        let reference = table.create(owner, target, 0x8000, true).unwrap();
        assert_eq!(table.cleanup_crashed_domain(target), [None; MAX_GRANTS]);
        assert!(!table.references_domain(target));
        assert_eq!(
            table.revoke(owner, reference),
            Err(GrantError::UnknownGrant)
        );
    }
}
