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
        if owner == target {
            return Err(GrantError::InvalidTarget);
        }
        if host_page == 0 || host_page & 0xfff != 0 {
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
        if self.target_page_is_mapped(target, target_page) {
            return Err(GrantError::AlreadyMapped);
        }
        let grant = self.grant_mut(reference)?;
        if grant.target != target {
            return Err(GrantError::AccessDenied);
        }
        if grant.target_page.is_some() {
            return Err(GrantError::AlreadyMapped);
        }
        grant.target_page = Some(target_page);
        Ok(GrantMapping {
            owner: grant.owner,
            target,
            host_page: grant.host_page,
            target_page,
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

    pub fn cleanup_domain(&mut self, domain: DomainId) -> [Option<GrantMapping>; MAX_GRANTS] {
        let mut mappings = [None; MAX_GRANTS];
        let mut mapping_count = 0;
        for slot in &mut self.grants {
            let Some(grant) = slot.grant else {
                continue;
            };
            if grant.owner != domain && grant.target != domain {
                continue;
            }
            if let Some(target_page) = grant.target_page {
                mappings[mapping_count] = Some(GrantMapping {
                    owner: grant.owner,
                    target: grant.target,
                    host_page: grant.host_page,
                    target_page,
                    writable: grant.writable,
                });
                mapping_count += 1;
            }
            slot.grant = None;
        }
        mappings
    }

    pub fn target_page_is_mapped(&self, target: DomainId, target_page: u64) -> bool {
        self.grants
            .iter()
            .filter_map(|slot| slot.grant.as_ref())
            .any(|grant| grant.target == target && grant.target_page == Some(target_page))
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
}
