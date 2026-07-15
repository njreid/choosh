//! Bounded, deterministic lifecycle state for explicitly declared services.
//!
//! A declaration fixes the only host destination a service may use. Process control,
//! readiness probes, and loopback gateways are outer capabilities; this module records
//! their outcomes without inferring ports or coupling client pins to process lifetime.

use std::collections::{BTreeMap, BTreeSet};

use crate::item::{ItemId, LoopbackPort, ServiceProtocol};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceDeclaration {
    pub id: ItemId,
    pub protocol: ServiceProtocol,
    pub port: LoopbackPort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Starting,
    Running,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Readiness {
    NotChecked,
    NotReady,
    Ready,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceState {
    declaration: ServiceDeclaration,
    status: ServiceStatus,
    readiness: Readiness,
    generation: u64,
}

impl ServiceState {
    #[must_use]
    pub const fn declaration(&self) -> &ServiceDeclaration {
        &self.declaration
    }

    #[must_use]
    pub const fn status(&self) -> ServiceStatus {
        self.status
    }

    #[must_use]
    pub const fn readiness(&self) -> Readiness {
        self.readiness
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceError {
    CapacityExceeded,
    DuplicateService,
    ServiceNotFound,
    AlreadyActive,
    NotActive,
    StaleGeneration { expected: u64, actual: u64 },
    GenerationExhausted,
}

#[derive(Debug)]
pub struct ServiceRegistry {
    capacity: usize,
    services: BTreeMap<ItemId, ServiceState>,
}

impl ServiceRegistry {
    /// Creates a registry with a strict service bound.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::CapacityExceeded`] when `capacity` is zero.
    pub fn new(capacity: usize) -> Result<Self, ServiceError> {
        if capacity == 0 {
            return Err(ServiceError::CapacityExceeded);
        }
        Ok(Self {
            capacity,
            services: BTreeMap::new(),
        })
    }

    /// Registers stopped metadata for one exact declared protocol and port.
    ///
    /// # Errors
    ///
    /// Returns a typed duplicate or capacity error without changing the registry.
    pub fn register(&mut self, declaration: ServiceDeclaration) -> Result<(), ServiceError> {
        if self.services.contains_key(&declaration.id) {
            return Err(ServiceError::DuplicateService);
        }
        if self.services.len() >= self.capacity {
            return Err(ServiceError::CapacityExceeded);
        }
        self.services.insert(
            declaration.id.clone(),
            ServiceState {
                declaration,
                status: ServiceStatus::Stopped,
                readiness: Readiness::NotChecked,
                generation: 0,
            },
        );
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &ItemId) -> Option<&ServiceState> {
        self.services.get(id)
    }

    /// Begins an explicit start and returns the new stable process generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or already-active service, or generation overflow.
    pub fn start(&mut self, id: &ItemId) -> Result<u64, ServiceError> {
        let service = self
            .services
            .get_mut(id)
            .ok_or(ServiceError::ServiceNotFound)?;
        if matches!(
            service.status,
            ServiceStatus::Starting | ServiceStatus::Running
        ) {
            return Err(ServiceError::AlreadyActive);
        }
        let generation = service
            .generation
            .checked_add(1)
            .ok_or(ServiceError::GenerationExhausted)?;
        service.generation = generation;
        service.status = ServiceStatus::Starting;
        service.readiness = Readiness::NotChecked;
        Ok(generation)
    }

    /// Applies a readiness observation only to the matching process generation.
    ///
    /// A ready observation promotes `starting` to `running`; a negative observation is
    /// informational and never substitutes a different destination port.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, inactive, or stale generation.
    pub fn observe_readiness(
        &mut self,
        id: &ItemId,
        generation: u64,
        ready: bool,
    ) -> Result<(), ServiceError> {
        let service = self.active_generation_mut(id, generation)?;
        service.readiness = if ready {
            Readiness::Ready
        } else {
            Readiness::NotReady
        };
        if ready && service.status == ServiceStatus::Starting {
            service.status = ServiceStatus::Running;
        }
        Ok(())
    }

    /// Records an explicit process failure for the matching generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, inactive, or stale generation.
    pub fn fail(&mut self, id: &ItemId, generation: u64) -> Result<(), ServiceError> {
        let service = self.active_generation_mut(id, generation)?;
        service.status = ServiceStatus::Failed;
        service.readiness = Readiness::NotReady;
        Ok(())
    }

    /// Marks active services unknown after host observation is lost, preserving generations.
    pub fn disconnected(&mut self) {
        for service in self.services.values_mut() {
            if matches!(
                service.status,
                ServiceStatus::Starting | ServiceStatus::Running
            ) {
                service.status = ServiceStatus::Unknown;
                service.readiness = Readiness::NotChecked;
            }
        }
    }

    /// Reconciles a retained process after reconnect without creating a new generation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or stale generation, or a service not awaiting
    /// reconciliation.
    pub fn reconnect(
        &mut self,
        id: &ItemId,
        generation: u64,
        running: bool,
    ) -> Result<(), ServiceError> {
        let service = self
            .services
            .get_mut(id)
            .ok_or(ServiceError::ServiceNotFound)?;
        check_generation(service, generation)?;
        if service.status != ServiceStatus::Unknown {
            return Err(ServiceError::NotActive);
        }
        service.status = if running {
            ServiceStatus::Starting
        } else {
            ServiceStatus::Stopped
        };
        service.readiness = Readiness::NotChecked;
        Ok(())
    }

    /// Records an explicit stop. Pin state is deliberately not an input to this operation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, inactive, or stale generation.
    pub fn stop(&mut self, id: &ItemId, generation: u64) -> Result<(), ServiceError> {
        let service = self.active_generation_mut(id, generation)?;
        service.status = ServiceStatus::Stopped;
        service.readiness = Readiness::NotChecked;
        Ok(())
    }

    fn active_generation_mut(
        &mut self,
        id: &ItemId,
        generation: u64,
    ) -> Result<&mut ServiceState, ServiceError> {
        let service = self
            .services
            .get_mut(id)
            .ok_or(ServiceError::ServiceNotFound)?;
        check_generation(service, generation)?;
        if matches!(
            service.status,
            ServiceStatus::Stopped | ServiceStatus::Failed
        ) {
            return Err(ServiceError::NotActive);
        }
        Ok(service)
    }
}

fn check_generation(service: &ServiceState, actual: u64) -> Result<(), ServiceError> {
    if service.generation == actual {
        Ok(())
    } else {
        Err(ServiceError::StaleGeneration {
            expected: service.generation,
            actual,
        })
    }
}

/// Client-owned pin identities, intentionally separate from [`ServiceRegistry`].
#[derive(Debug)]
pub struct ServicePins {
    capacity: usize,
    pinned: BTreeSet<ItemId>,
}

impl ServicePins {
    /// Creates a bounded pin set.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::CapacityExceeded`] when `capacity` is zero.
    pub fn new(capacity: usize) -> Result<Self, ServiceError> {
        if capacity == 0 {
            return Err(ServiceError::CapacityExceeded);
        }
        Ok(Self {
            capacity,
            pinned: BTreeSet::new(),
        })
    }

    /// Pins a stable service identity. Repeated pins are idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::CapacityExceeded`] without changing a full set.
    pub fn pin(&mut self, id: ItemId) -> Result<(), ServiceError> {
        if !self.pinned.contains(&id) && self.pinned.len() >= self.capacity {
            return Err(ServiceError::CapacityExceeded);
        }
        self.pinned.insert(id);
        Ok(())
    }

    /// Removes only client presentation state; it cannot stop a service.
    #[must_use]
    pub fn unpin(&mut self, id: &ItemId) -> bool {
        self.pinned.remove(id)
    }

    #[must_use]
    pub fn contains(&self, id: &ItemId) -> bool {
        self.pinned.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ItemId {
        ItemId::parse(value).expect("test identity is valid")
    }

    fn declaration(value: &str, port: u16) -> ServiceDeclaration {
        ServiceDeclaration {
            id: id(value),
            protocol: ServiceProtocol::Http,
            port: LoopbackPort::new(port).expect("test port is valid"),
        }
    }

    #[test]
    fn declaration_remains_exact_across_lifecycle() {
        let mut registry = ServiceRegistry::new(1).unwrap();
        registry.register(declaration("web", 3000)).unwrap();
        let generation = registry.start(&id("web")).unwrap();
        registry
            .observe_readiness(&id("web"), generation, true)
            .unwrap();

        let state = registry.get(&id("web")).unwrap();
        assert_eq!(state.status(), ServiceStatus::Running);
        assert_eq!(state.readiness(), Readiness::Ready);
        assert_eq!(state.declaration().port.get(), 3000);
        assert_eq!(state.declaration().protocol, ServiceProtocol::Http);
    }

    #[test]
    fn reconnect_retains_generation_and_rejects_stale_observations() {
        let mut registry = ServiceRegistry::new(1).unwrap();
        registry.register(declaration("web", 3000)).unwrap();
        let first = registry.start(&id("web")).unwrap();
        registry.disconnected();
        registry.reconnect(&id("web"), first, true).unwrap();
        registry.observe_readiness(&id("web"), first, true).unwrap();
        registry.stop(&id("web"), first).unwrap();
        let second = registry.start(&id("web")).unwrap();

        assert_eq!(second, first + 1);
        assert_eq!(
            registry.observe_readiness(&id("web"), first, true),
            Err(ServiceError::StaleGeneration {
                expected: second,
                actual: first,
            })
        );
    }

    #[test]
    fn pin_and_unpin_do_not_change_process_lifecycle() {
        let service_id = id("web");
        let mut registry = ServiceRegistry::new(1).unwrap();
        registry.register(declaration("web", 3000)).unwrap();
        let generation = registry.start(&service_id).unwrap();
        registry
            .observe_readiness(&service_id, generation, true)
            .unwrap();
        let mut pins = ServicePins::new(1).unwrap();

        pins.pin(service_id.clone()).unwrap();
        assert!(pins.unpin(&service_id));
        assert_eq!(
            registry.get(&service_id).unwrap().status(),
            ServiceStatus::Running
        );
    }

    #[test]
    fn bounds_fail_without_mutating_existing_entries() {
        let mut registry = ServiceRegistry::new(1).unwrap();
        registry.register(declaration("web", 3000)).unwrap();
        assert_eq!(
            registry.register(declaration("api", 4000)),
            Err(ServiceError::CapacityExceeded)
        );
        assert!(registry.get(&id("web")).is_some());
        assert!(registry.get(&id("api")).is_none());

        let mut pins = ServicePins::new(1).unwrap();
        pins.pin(id("web")).unwrap();
        assert_eq!(pins.pin(id("api")), Err(ServiceError::CapacityExceeded));
        assert!(pins.contains(&id("web")));
    }
}
