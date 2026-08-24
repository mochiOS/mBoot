use crate::memory::NestedPageTable;
use crate::{BackendKind, Error};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainId(u32);

impl DomainId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainState {
    Created,
    Ready,
    Running,
    Stopped,
    Crashed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainRole {
    System,
    Hardware,
    Application,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Domain {
    id: DomainId,
    role: DomainRole,
    capabilities: u64,
    backend: BackendKind,
    state: DomainState,
    nested_pages: NestedPageTable,
}

impl Domain {
    pub const fn new(
        id: DomainId,
        role: DomainRole,
        capabilities: u64,
        backend: BackendKind,
        nested_pages: NestedPageTable,
    ) -> Self {
        Self {
            id,
            role,
            capabilities,
            backend,
            state: DomainState::Created,
            nested_pages,
        }
    }

    pub const fn id(&self) -> DomainId {
        self.id
    }

    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    pub const fn role(&self) -> DomainRole {
        self.role
    }

    pub const fn capabilities(&self) -> u64 {
        self.capabilities
    }

    pub const fn state(&self) -> DomainState {
        self.state
    }

    pub const fn nested_pages(&self) -> &NestedPageTable {
        &self.nested_pages
    }

    pub fn mark_ready(&mut self) -> Result<(), Error> {
        self.transition(DomainState::Created, DomainState::Ready)
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.transition(DomainState::Ready, DomainState::Running)
    }

    pub fn stop(&mut self) -> Result<(), Error> {
        self.transition(DomainState::Running, DomainState::Stopped)
    }

    pub fn mark_crashed(&mut self) -> Result<(), Error> {
        if self.state != DomainState::Running {
            return Err(Error::InvalidState);
        }
        self.state = DomainState::Crashed;
        Ok(())
    }

    pub fn prepare_restart(&mut self) -> Result<(), Error> {
        if !matches!(self.state, DomainState::Crashed | DomainState::Stopped) {
            return Err(Error::InvalidState);
        }
        self.state = DomainState::Ready;
        Ok(())
    }

    fn transition(&mut self, from: DomainState, to: DomainState) -> Result<(), Error> {
        if self.state != from {
            return Err(Error::InvalidState);
        }
        self.state = to;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_state_machine_rejects_skipped_states() {
        let pages = NestedPageTable::test_new(BackendKind::IntelVmx, 0x1000, 0x5000, 1);
        let mut domain = Domain::new(
            DomainId::new(1),
            DomainRole::System,
            0,
            BackendKind::IntelVmx,
            pages,
        );
        assert_eq!(domain.start(), Err(Error::InvalidState));
        assert_eq!(domain.mark_ready(), Ok(()));
        assert_eq!(domain.start(), Ok(()));
        assert_eq!(domain.stop(), Ok(()));
    }

    #[test]
    fn crashed_domain_must_be_prepared_before_restart() {
        let pages = NestedPageTable::test_new(BackendKind::IntelVmx, 0x1000, 0x5000, 1);
        let mut domain = Domain::new(
            DomainId::new(3),
            DomainRole::Application,
            0,
            BackendKind::IntelVmx,
            pages,
        );
        domain.mark_ready().unwrap();
        domain.start().unwrap();
        domain.mark_crashed().unwrap();
        assert_eq!(domain.start(), Err(Error::InvalidState));
        assert_eq!(domain.prepare_restart(), Ok(()));
        assert_eq!(domain.start(), Ok(()));
    }
}
