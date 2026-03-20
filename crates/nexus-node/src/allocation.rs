//! Port allocation management for game servers
//!
//! Manages network port allocations for containers:
//! - Allocate and deallocate ports
//! - Track port usage per container
//! - Support for primary and additional allocations
//! - IP address binding

use crate::error::{NodeError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

/// Port allocation information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    /// Unique allocation ID
    pub id: String,
    /// IP address to bind to
    pub ip: IpAddr,
    /// Port number
    pub port: u16,
    /// Optional alias for this allocation
    pub alias: Option<String>,
    /// Optional notes
    pub notes: Option<String>,
    /// Whether this is the primary allocation
    pub is_primary: bool,
    /// Container ID this allocation is assigned to (if any)
    pub container_id: Option<String>,
}

impl Allocation {
    /// Get the socket address string
    pub fn address(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }
}

/// Port range for allocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortRange {
    /// Start port (inclusive)
    pub start: u16,
    /// End port (inclusive)
    pub end: u16,
}

impl PortRange {
    /// Create a new port range
    pub fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    /// Check if a port is within this range
    pub fn contains(&self, port: u16) -> bool {
        port >= self.start && port <= self.end
    }

    /// Get the number of ports in this range
    pub fn size(&self) -> u16 {
        self.end.saturating_sub(self.start) + 1
    }
}

/// Allocation pool for an IP address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationPool {
    /// IP address for this pool
    pub ip: IpAddr,
    /// Port ranges available for allocation
    pub port_ranges: Vec<PortRange>,
    /// Ports that are explicitly blocked
    pub blocked_ports: HashSet<u16>,
}

impl AllocationPool {
    /// Create a new allocation pool
    pub fn new(ip: IpAddr) -> Self {
        Self {
            ip,
            port_ranges: Vec::new(),
            blocked_ports: HashSet::new(),
        }
    }

    /// Add a port range to the pool
    pub fn add_range(&mut self, start: u16, end: u16) {
        self.port_ranges.push(PortRange::new(start, end));
    }

    /// Block a specific port
    pub fn block_port(&mut self, port: u16) {
        self.blocked_ports.insert(port);
    }

    /// Unblock a specific port
    pub fn unblock_port(&mut self, port: u16) {
        self.blocked_ports.remove(&port);
    }

    /// Check if a port is available for allocation
    pub fn is_port_available(&self, port: u16, allocated: &HashSet<u16>) -> bool {
        // Check if blocked
        if self.blocked_ports.contains(&port) {
            return false;
        }

        // Check if already allocated
        if allocated.contains(&port) {
            return false;
        }

        // Check if within any port range
        self.port_ranges.iter().any(|range| range.contains(port))
    }

    /// Find the next available port in the pool
    pub fn find_available_port(&self, allocated: &HashSet<u16>) -> Option<u16> {
        for range in &self.port_ranges {
            for port in range.start..=range.end {
                if self.is_port_available(port, allocated) {
                    return Some(port);
                }
            }
        }
        None
    }
}

/// Allocation manager for handling port allocations
pub struct AllocationManager {
    /// Allocation pools indexed by IP address
    pools: Arc<RwLock<HashMap<IpAddr, AllocationPool>>>,
    /// All allocations indexed by allocation ID
    allocations: Arc<RwLock<HashMap<String, Allocation>>>,
    /// Allocations by container ID
    container_allocations: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Allocated ports per IP
    allocated_ports: Arc<RwLock<HashMap<IpAddr, HashSet<u16>>>>,
}

impl AllocationManager {
    /// Create a new allocation manager
    pub fn new() -> Self {
        Self {
            pools: Arc::new(RwLock::new(HashMap::new())),
            allocations: Arc::new(RwLock::new(HashMap::new())),
            container_allocations: Arc::new(RwLock::new(HashMap::new())),
            allocated_ports: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add an allocation pool
    pub async fn add_pool(&self, pool: AllocationPool) -> Result<()> {
        let ip = pool.ip;
        let mut pools = self.pools.write().await;
        pools.insert(ip, pool);
        info!("Added allocation pool for IP {}", ip);
        Ok(())
    }

    /// Create a new allocation pool with port range
    pub async fn create_pool(&self, ip: IpAddr, start_port: u16, end_port: u16) -> Result<()> {
        let mut pool = AllocationPool::new(ip);
        pool.add_range(start_port, end_port);
        self.add_pool(pool).await
    }

    /// Create a new allocation
    pub async fn create_allocation(
        &self,
        ip: IpAddr,
        port: Option<u16>,
        alias: Option<&str>,
        notes: Option<&str>,
    ) -> Result<Allocation> {
        let pools = self.pools.read().await;
        let pool = pools.get(&ip).ok_or_else(|| {
            NodeError::InvalidInput(format!("No allocation pool found for IP {}", ip))
        })?;

        let mut allocated_ports = self.allocated_ports.write().await;
        let ip_allocated = allocated_ports.entry(ip).or_insert_with(HashSet::new);

        // Find or validate port
        let port = if let Some(p) = port {
            if !pool.is_port_available(p, ip_allocated) {
                return Err(NodeError::InvalidInput(format!(
                    "Port {} is not available on IP {}",
                    p, ip
                )));
            }
            p
        } else {
            pool.find_available_port(ip_allocated).ok_or_else(|| {
                NodeError::InvalidInput(format!("No available ports on IP {}", ip))
            })?
        };

        let allocation_id = Uuid::new_v4().to_string();
        let allocation = Allocation {
            id: allocation_id.clone(),
            ip,
            port,
            alias: alias.map(|s| s.to_string()),
            notes: notes.map(|s| s.to_string()),
            is_primary: false,
            container_id: None,
        };

        // Mark port as allocated
        ip_allocated.insert(port);

        // Store allocation
        {
            let mut allocations = self.allocations.write().await;
            allocations.insert(allocation_id.clone(), allocation.clone());
        }

        info!("Created allocation {} ({}:{})", allocation_id, ip, port);

        Ok(allocation)
    }

    /// Assign an allocation to a container
    pub async fn assign_allocation(
        &self,
        allocation_id: &str,
        container_id: &str,
        is_primary: bool,
    ) -> Result<Allocation> {
        let mut allocations = self.allocations.write().await;
        let allocation = allocations.get_mut(allocation_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Allocation {} not found", allocation_id))
        })?;

        if allocation.container_id.is_some() {
            return Err(NodeError::InvalidInput(format!(
                "Allocation {} is already assigned",
                allocation_id
            )));
        }

        allocation.container_id = Some(container_id.to_string());
        allocation.is_primary = is_primary;

        // Update container allocations index
        {
            let mut container_allocations = self.container_allocations.write().await;
            container_allocations
                .entry(container_id.to_string())
                .or_insert_with(Vec::new)
                .push(allocation_id.to_string());
        }

        info!(
            "Assigned allocation {} to container {} (primary: {})",
            allocation_id, container_id, is_primary
        );

        Ok(allocation.clone())
    }

    /// Unassign an allocation from a container
    pub async fn unassign_allocation(&self, allocation_id: &str) -> Result<Allocation> {
        let mut allocations = self.allocations.write().await;
        let allocation = allocations.get_mut(allocation_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Allocation {} not found", allocation_id))
        })?;

        let container_id = allocation.container_id.take();
        allocation.is_primary = false;

        // Update container allocations index
        if let Some(ref cid) = container_id {
            let mut container_allocations = self.container_allocations.write().await;
            if let Some(allocs) = container_allocations.get_mut(cid) {
                allocs.retain(|id| id != allocation_id);
            }
        }

        info!("Unassigned allocation {}", allocation_id);

        Ok(allocation.clone())
    }

    /// Delete an allocation
    pub async fn delete_allocation(&self, allocation_id: &str) -> Result<()> {
        // Remove from allocations
        let allocation = {
            let mut allocations = self.allocations.write().await;
            allocations.remove(allocation_id).ok_or_else(|| {
                NodeError::InvalidInput(format!("Allocation {} not found", allocation_id))
            })?
        };

        // Free the port
        {
            let mut allocated_ports = self.allocated_ports.write().await;
            if let Some(ip_allocated) = allocated_ports.get_mut(&allocation.ip) {
                ip_allocated.remove(&allocation.port);
            }
        }

        // Remove from container allocations if assigned
        if let Some(ref container_id) = allocation.container_id {
            let mut container_allocations = self.container_allocations.write().await;
            if let Some(allocs) = container_allocations.get_mut(container_id) {
                allocs.retain(|id| id != allocation_id);
            }
        }

        info!(
            "Deleted allocation {} ({}:{})",
            allocation_id, allocation.ip, allocation.port
        );

        Ok(())
    }

    /// Get an allocation by ID
    pub async fn get_allocation(&self, allocation_id: &str) -> Result<Allocation> {
        let allocations = self.allocations.read().await;
        allocations.get(allocation_id).cloned().ok_or_else(|| {
            NodeError::InvalidInput(format!("Allocation {} not found", allocation_id))
        })
    }

    /// List all allocations for a container
    pub async fn list_container_allocations(&self, container_id: &str) -> Result<Vec<Allocation>> {
        let container_allocations = self.container_allocations.read().await;
        let allocations = self.allocations.read().await;

        let mut result = Vec::new();

        if let Some(alloc_ids) = container_allocations.get(container_id) {
            for alloc_id in alloc_ids {
                if let Some(alloc) = allocations.get(alloc_id) {
                    result.push(alloc.clone());
                }
            }
        }

        // Sort by primary first, then by port
        result.sort_by(|a, b| {
            if a.is_primary != b.is_primary {
                b.is_primary.cmp(&a.is_primary)
            } else {
                a.port.cmp(&b.port)
            }
        });

        Ok(result)
    }

    /// Get the primary allocation for a container
    pub async fn get_primary_allocation(&self, container_id: &str) -> Option<Allocation> {
        let container_allocations = self.container_allocations.read().await;
        let allocations = self.allocations.read().await;

        if let Some(alloc_ids) = container_allocations.get(container_id) {
            for alloc_id in alloc_ids {
                if let Some(alloc) = allocations.get(alloc_id) {
                    if alloc.is_primary {
                        return Some(alloc.clone());
                    }
                }
            }
        }

        None
    }

    /// List all unassigned allocations
    pub async fn list_unassigned_allocations(&self) -> Vec<Allocation> {
        let allocations = self.allocations.read().await;
        allocations.values().filter(|a| a.container_id.is_none()).cloned().collect()
    }

    /// List all allocations for an IP
    pub async fn list_ip_allocations(&self, ip: IpAddr) -> Vec<Allocation> {
        let allocations = self.allocations.read().await;
        allocations.values().filter(|a| a.ip == ip).cloned().collect()
    }

    /// Get allocation statistics
    pub async fn get_stats(&self) -> AllocationStats {
        let pools = self.pools.read().await;
        let allocations = self.allocations.read().await;
        let allocated_ports = self.allocated_ports.read().await;

        let mut total_ports = 0u32;
        let mut used_ports = 0u32;

        for (ip, pool) in pools.iter() {
            for range in &pool.port_ranges {
                total_ports += range.size() as u32;
            }
            if let Some(allocated) = allocated_ports.get(ip) {
                used_ports += allocated.len() as u32;
            }
        }

        let assigned = allocations.values().filter(|a| a.container_id.is_some()).count() as u32;

        AllocationStats {
            total_pools: pools.len() as u32,
            total_ports,
            used_ports,
            available_ports: total_ports.saturating_sub(used_ports),
            assigned_allocations: assigned,
            unassigned_allocations: allocations.len() as u32 - assigned,
        }
    }

    /// Update allocation details
    pub async fn update_allocation(
        &self,
        allocation_id: &str,
        alias: Option<&str>,
        notes: Option<&str>,
    ) -> Result<Allocation> {
        let mut allocations = self.allocations.write().await;
        let allocation = allocations.get_mut(allocation_id).ok_or_else(|| {
            NodeError::InvalidInput(format!("Allocation {} not found", allocation_id))
        })?;

        if let Some(a) = alias {
            allocation.alias = Some(a.to_string());
        }
        if let Some(n) = notes {
            allocation.notes = Some(n.to_string());
        }

        Ok(allocation.clone())
    }

    /// Auto-allocate ports for a container
    pub async fn auto_allocate(
        &self,
        container_id: &str,
        count: usize,
        ip: Option<IpAddr>,
    ) -> Result<Vec<Allocation>> {
        let pools = self.pools.read().await;

        // Find suitable pool
        let target_ip = if let Some(ip) = ip {
            if !pools.contains_key(&ip) {
                return Err(NodeError::InvalidInput(format!(
                    "No pool found for IP {}",
                    ip
                )));
            }
            ip
        } else {
            // Use first available pool
            *pools.keys().next().ok_or_else(|| {
                NodeError::InvalidInput("No allocation pools configured".to_string())
            })?
        };

        drop(pools);

        let mut allocations = Vec::new();

        for i in 0..count {
            let alloc = self.create_allocation(target_ip, None, None, None).await?;
            let is_primary = i == 0;
            let assigned = self.assign_allocation(&alloc.id, container_id, is_primary).await?;
            allocations.push(assigned);
        }

        info!(
            "Auto-allocated {} ports for container {} on IP {}",
            count, container_id, target_ip
        );

        Ok(allocations)
    }
}

impl Default for AllocationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Allocation statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationStats {
    /// Number of allocation pools
    pub total_pools: u32,
    /// Total ports available across all pools
    pub total_ports: u32,
    /// Ports currently allocated
    pub used_ports: u32,
    /// Ports available for allocation
    pub available_ports: u32,
    /// Allocations assigned to containers
    pub assigned_allocations: u32,
    /// Allocations not assigned to any container
    pub unassigned_allocations: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_port_range() {
        let range = PortRange::new(25565, 25575);
        assert!(range.contains(25565));
        assert!(range.contains(25570));
        assert!(range.contains(25575));
        assert!(!range.contains(25564));
        assert!(!range.contains(25576));
        assert_eq!(range.size(), 11);
    }

    #[tokio::test]
    async fn test_create_pool() {
        let manager = AllocationManager::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.create_pool(ip, 25565, 25575).await.unwrap();

        let stats = manager.get_stats().await;
        assert_eq!(stats.total_pools, 1);
        assert_eq!(stats.total_ports, 11);
    }

    #[tokio::test]
    async fn test_create_allocation() {
        let manager = AllocationManager::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.create_pool(ip, 25565, 25575).await.unwrap();

        let alloc = manager
            .create_allocation(ip, Some(25565), Some("Game Port"), None)
            .await
            .unwrap();

        assert_eq!(alloc.ip, ip);
        assert_eq!(alloc.port, 25565);
        assert_eq!(alloc.alias, Some("Game Port".to_string()));
    }

    #[tokio::test]
    async fn test_auto_allocate() {
        let manager = AllocationManager::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.create_pool(ip, 25565, 25575).await.unwrap();

        let allocs = manager.auto_allocate("container-1", 3, Some(ip)).await.unwrap();

        assert_eq!(allocs.len(), 3);
        assert!(allocs[0].is_primary);
        assert!(!allocs[1].is_primary);
        assert!(!allocs[2].is_primary);

        // All should have different ports
        let ports: HashSet<u16> = allocs.iter().map(|a| a.port).collect();
        assert_eq!(ports.len(), 3);
    }

    #[tokio::test]
    async fn test_assign_unassign() {
        let manager = AllocationManager::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.create_pool(ip, 25565, 25575).await.unwrap();

        let alloc = manager.create_allocation(ip, None, None, None).await.unwrap();

        // Assign
        let assigned = manager.assign_allocation(&alloc.id, "container-1", true).await.unwrap();
        assert_eq!(assigned.container_id, Some("container-1".to_string()));
        assert!(assigned.is_primary);

        // List container allocations
        let container_allocs = manager.list_container_allocations("container-1").await.unwrap();
        assert_eq!(container_allocs.len(), 1);

        // Unassign
        let unassigned = manager.unassign_allocation(&alloc.id).await.unwrap();
        assert!(unassigned.container_id.is_none());
        assert!(!unassigned.is_primary);
    }

    #[tokio::test]
    async fn test_delete_allocation() {
        let manager = AllocationManager::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));

        manager.create_pool(ip, 25565, 25575).await.unwrap();

        let alloc = manager.create_allocation(ip, Some(25565), None, None).await.unwrap();

        // Delete
        manager.delete_allocation(&alloc.id).await.unwrap();

        // Port should be available again
        let alloc2 = manager.create_allocation(ip, Some(25565), None, None).await.unwrap();
        assert_eq!(alloc2.port, 25565);
    }
}
