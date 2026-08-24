use crate::domain::DomainId;

pub const MAX_EVENT_CHANNELS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventError {
    InvalidEndpoint,
    DuplicatePort,
    TableFull,
    UnboundPort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Endpoint {
    domain: DomainId,
    port: u32,
    pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventChannel {
    a: Endpoint,
    b: Endpoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventDelivery {
    pub domain: DomainId,
    pub port: u32,
}

pub struct EventChannelTable {
    channels: [Option<EventChannel>; MAX_EVENT_CHANNELS],
}

impl EventChannelTable {
    pub const fn new() -> Self {
        Self {
            channels: [None; MAX_EVENT_CHANNELS],
        }
    }

    pub fn connect(
        &mut self,
        domain_a: DomainId,
        port_a: u32,
        domain_b: DomainId,
        port_b: u32,
    ) -> Result<(), EventError> {
        if domain_a == domain_b || port_a == 0 || port_b == 0 {
            return Err(EventError::InvalidEndpoint);
        }
        if self.endpoint_exists(domain_a, port_a) || self.endpoint_exists(domain_b, port_b) {
            return Err(EventError::DuplicatePort);
        }
        let Some(slot) = self.channels.iter_mut().find(|slot| slot.is_none()) else {
            return Err(EventError::TableFull);
        };
        *slot = Some(EventChannel {
            a: Endpoint {
                domain: domain_a,
                port: port_a,
                pending: false,
            },
            b: Endpoint {
                domain: domain_b,
                port: port_b,
                pending: false,
            },
        });
        Ok(())
    }

    pub fn send(&mut self, sender: DomainId, port: u32) -> Result<EventDelivery, EventError> {
        for channel in self.channels.iter_mut().flatten() {
            if channel.a.domain == sender && channel.a.port == port {
                channel.b.pending = true;
                return Ok(EventDelivery {
                    domain: channel.b.domain,
                    port: channel.b.port,
                });
            }
            if channel.b.domain == sender && channel.b.port == port {
                channel.a.pending = true;
                return Ok(EventDelivery {
                    domain: channel.a.domain,
                    port: channel.a.port,
                });
            }
        }
        Err(EventError::UnboundPort)
    }

    pub fn receive(&mut self, receiver: DomainId) -> Option<u32> {
        for channel in self.channels.iter_mut().flatten() {
            for endpoint in [&mut channel.a, &mut channel.b] {
                if endpoint.domain == receiver && endpoint.pending {
                    endpoint.pending = false;
                    return Some(endpoint.port);
                }
            }
        }
        None
    }

    pub fn has_pending(&self, receiver: DomainId) -> bool {
        self.channels.iter().flatten().any(|channel| {
            (channel.a.domain == receiver && channel.a.pending)
                || (channel.b.domain == receiver && channel.b.pending)
        })
    }

    pub fn disconnect_domain(&mut self, domain: DomainId) -> usize {
        let mut disconnected = 0;
        for slot in &mut self.channels {
            if slot.is_some_and(|channel| channel.a.domain == domain || channel.b.domain == domain)
            {
                *slot = None;
                disconnected += 1;
            }
        }
        disconnected
    }

    fn endpoint_exists(&self, domain: DomainId, port: u32) -> bool {
        self.channels.iter().flatten().any(|channel| {
            (channel.a.domain == domain && channel.a.port == port)
                || (channel.b.domain == domain && channel.b.port == port)
        })
    }
}

impl Default for EventChannelTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_only_across_a_connected_port() {
        let mut table = EventChannelTable::new();
        table
            .connect(DomainId::new(1), 7, DomainId::new(2), 11)
            .unwrap();
        assert_eq!(
            table.send(DomainId::new(1), 7),
            Ok(EventDelivery {
                domain: DomainId::new(2),
                port: 11,
            })
        );
        assert_eq!(table.receive(DomainId::new(1)), None);
        assert_eq!(table.receive(DomainId::new(2)), Some(11));
        assert_eq!(
            table.send(DomainId::new(2), 11),
            Ok(EventDelivery {
                domain: DomainId::new(1),
                port: 7,
            })
        );
        assert_eq!(table.receive(DomainId::new(1)), Some(7));
        assert_eq!(
            table.send(DomainId::new(3), 7),
            Err(EventError::UnboundPort)
        );
    }

    #[test]
    fn repeated_notifications_are_coalesced() {
        let mut table = EventChannelTable::new();
        table
            .connect(DomainId::new(1), 1, DomainId::new(2), 1)
            .unwrap();
        assert!(table.send(DomainId::new(1), 1).is_ok());
        assert!(table.send(DomainId::new(1), 1).is_ok());
        assert!(table.has_pending(DomainId::new(2)));
        assert_eq!(table.receive(DomainId::new(2)), Some(1));
        assert!(!table.has_pending(DomainId::new(2)));
        assert_eq!(table.receive(DomainId::new(2)), None);
    }

    #[test]
    fn refuses_duplicate_local_ports() {
        let mut table = EventChannelTable::new();
        table
            .connect(DomainId::new(1), 1, DomainId::new(2), 1)
            .unwrap();
        assert_eq!(
            table.connect(DomainId::new(1), 1, DomainId::new(3), 1),
            Err(EventError::DuplicatePort)
        );
    }

    #[test]
    fn disconnecting_a_domain_removes_both_endpoints() {
        let mut table = EventChannelTable::new();
        table
            .connect(DomainId::new(1), 1, DomainId::new(2), 1)
            .unwrap();
        table
            .connect(DomainId::new(2), 2, DomainId::new(3), 1)
            .unwrap();
        assert_eq!(table.disconnect_domain(DomainId::new(2)), 2);
        assert_eq!(
            table.send(DomainId::new(1), 1),
            Err(EventError::UnboundPort)
        );
        assert_eq!(
            table.send(DomainId::new(3), 1),
            Err(EventError::UnboundPort)
        );
    }
}
