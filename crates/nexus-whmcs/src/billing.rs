//! Usage-based billing for WHMCS
//!
//! Provides:
//! - CPU/memory/disk/bandwidth usage tracking
//! - Automatic invoice generation
//! - Overage billing
//! - Usage reports

use crate::api::{InvoiceItem, WhmcsApi};
use crate::error::{Result, WhmcsError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Usage record for a server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Server ID
    pub server_id: String,
    /// Service ID in WHMCS
    pub service_id: u64,
    /// Client ID in WHMCS
    pub client_id: u64,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// CPU usage percentage (0-100 per core, can exceed 100 for multi-core)
    pub cpu_percent: f64,
    /// Memory used in bytes
    pub memory_bytes: u64,
    /// Disk used in bytes
    pub disk_bytes: u64,
    /// Network ingress in bytes
    pub network_in_bytes: u64,
    /// Network egress in bytes
    pub network_out_bytes: u64,
    /// Number of active players/connections
    pub active_connections: u32,
}

/// Billing rate configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingRates {
    /// Price per GB of RAM per hour
    pub ram_per_gb_hour: f64,
    /// Price per CPU core per hour
    pub cpu_per_core_hour: f64,
    /// Price per GB of disk per month
    pub disk_per_gb_month: f64,
    /// Price per GB of bandwidth
    pub bandwidth_per_gb: f64,
    /// Price per slot per month
    pub slot_per_month: f64,
    /// Free tier limits (included in base price)
    pub free_tier: FreeTierLimits,
}

impl Default for BillingRates {
    fn default() -> Self {
        Self {
            ram_per_gb_hour: 0.005,
            cpu_per_core_hour: 0.01,
            disk_per_gb_month: 0.10,
            bandwidth_per_gb: 0.01,
            slot_per_month: 0.50,
            free_tier: FreeTierLimits::default(),
        }
    }
}

/// Free tier limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreeTierLimits {
    /// Free RAM in bytes
    pub ram_bytes: u64,
    /// Free CPU percentage
    pub cpu_percent: f64,
    /// Free disk in bytes
    pub disk_bytes: u64,
    /// Free bandwidth in bytes
    pub bandwidth_bytes: u64,
    /// Free slots
    pub slots: u32,
}

impl Default for FreeTierLimits {
    fn default() -> Self {
        Self {
            ram_bytes: 1024 * 1024 * 1024,       // 1 GB
            cpu_percent: 100.0,                   // 1 core
            disk_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            bandwidth_bytes: 100 * 1024 * 1024 * 1024, // 100 GB
            slots: 10,
        }
    }
}

/// Usage summary for billing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    /// Service ID
    pub service_id: u64,
    /// Client ID
    pub client_id: u64,
    /// Period start
    pub period_start: DateTime<Utc>,
    /// Period end
    pub period_end: DateTime<Utc>,
    /// Average CPU usage
    pub avg_cpu_percent: f64,
    /// Peak CPU usage
    pub peak_cpu_percent: f64,
    /// Average memory in bytes
    pub avg_memory_bytes: u64,
    /// Peak memory in bytes
    pub peak_memory_bytes: u64,
    /// Disk used in bytes
    pub disk_bytes: u64,
    /// Total bandwidth in bytes
    pub total_bandwidth_bytes: u64,
    /// Peak active connections
    pub peak_connections: u32,
    /// Number of samples
    pub sample_count: u32,
}

/// Billing manager for usage-based billing
pub struct BillingManager {
    api: Arc<WhmcsApi>,
    rates: BillingRates,
    /// Usage records by service ID
    usage_records: Arc<RwLock<HashMap<u64, Vec<UsageRecord>>>>,
    /// Last billing timestamp by service ID
    last_billed: Arc<RwLock<HashMap<u64, DateTime<Utc>>>>,
}

impl BillingManager {
    /// Create a new billing manager
    pub fn new(api: Arc<WhmcsApi>, rates: BillingRates) -> Self {
        Self {
            api,
            rates,
            usage_records: Arc::new(RwLock::new(HashMap::new())),
            last_billed: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record usage for a server
    pub async fn record_usage(&self, record: UsageRecord) -> Result<()> {
        let service_id = record.service_id;

        let mut records = self.usage_records.write().await;
        records
            .entry(service_id)
            .or_insert_with(Vec::new)
            .push(record);

        debug!("Recorded usage for service {}", service_id);
        Ok(())
    }

    /// Get usage summary for a service
    pub async fn get_usage_summary(
        &self,
        service_id: u64,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<UsageSummary> {
        let records = self.usage_records.read().await;
        let service_records = records.get(&service_id).ok_or_else(|| {
            WhmcsError::BillingError(format!("No usage records for service {}", service_id))
        })?;

        // Filter records within time range
        let filtered: Vec<&UsageRecord> = service_records
            .iter()
            .filter(|r| r.timestamp >= start && r.timestamp <= end)
            .collect();

        if filtered.is_empty() {
            return Err(WhmcsError::BillingError(
                "No usage records in specified period".to_string(),
            ));
        }

        let client_id = filtered[0].client_id;
        let sample_count = filtered.len() as u32;

        // Calculate averages and peaks
        let avg_cpu: f64 = filtered.iter().map(|r| r.cpu_percent).sum::<f64>() / filtered.len() as f64;
        let peak_cpu = filtered
            .iter()
            .map(|r| r.cpu_percent)
            .fold(f64::MIN, f64::max);

        let avg_memory: u64 =
            filtered.iter().map(|r| r.memory_bytes).sum::<u64>() / filtered.len() as u64;
        let peak_memory = filtered.iter().map(|r| r.memory_bytes).max().unwrap_or(0);

        let disk_bytes = filtered.last().map(|r| r.disk_bytes).unwrap_or(0);

        let total_bandwidth: u64 = filtered
            .iter()
            .map(|r| r.network_in_bytes + r.network_out_bytes)
            .sum();

        let peak_connections = filtered
            .iter()
            .map(|r| r.active_connections)
            .max()
            .unwrap_or(0);

        Ok(UsageSummary {
            service_id,
            client_id,
            period_start: start,
            period_end: end,
            avg_cpu_percent: avg_cpu,
            peak_cpu_percent: peak_cpu,
            avg_memory_bytes: avg_memory,
            peak_memory_bytes: peak_memory,
            disk_bytes,
            total_bandwidth_bytes: total_bandwidth,
            peak_connections,
            sample_count,
        })
    }

    /// Calculate charges for usage
    pub fn calculate_charges(&self, summary: &UsageSummary) -> Vec<BillingCharge> {
        let mut charges = Vec::new();
        let hours = (summary.period_end - summary.period_start).num_hours() as f64;

        // RAM overage
        if summary.peak_memory_bytes > self.rates.free_tier.ram_bytes {
            let overage_gb =
                (summary.peak_memory_bytes - self.rates.free_tier.ram_bytes) as f64 / 1024.0 / 1024.0 / 1024.0;
            let charge = overage_gb * self.rates.ram_per_gb_hour * hours;
            if charge > 0.01 {
                charges.push(BillingCharge {
                    description: format!("RAM Overage ({:.2} GB peak)", overage_gb),
                    amount: charge,
                    quantity: overage_gb,
                    unit: "GB-hours".to_string(),
                });
            }
        }

        // CPU overage
        if summary.peak_cpu_percent > self.rates.free_tier.cpu_percent {
            let overage_cores =
                (summary.peak_cpu_percent - self.rates.free_tier.cpu_percent) / 100.0;
            let charge = overage_cores * self.rates.cpu_per_core_hour * hours;
            if charge > 0.01 {
                charges.push(BillingCharge {
                    description: format!("CPU Overage ({:.2} cores)", overage_cores),
                    amount: charge,
                    quantity: overage_cores,
                    unit: "core-hours".to_string(),
                });
            }
        }

        // Bandwidth overage
        if summary.total_bandwidth_bytes > self.rates.free_tier.bandwidth_bytes {
            let overage_gb = (summary.total_bandwidth_bytes - self.rates.free_tier.bandwidth_bytes)
                as f64
                / 1024.0
                / 1024.0
                / 1024.0;
            let charge = overage_gb * self.rates.bandwidth_per_gb;
            if charge > 0.01 {
                charges.push(BillingCharge {
                    description: format!("Bandwidth Overage ({:.2} GB)", overage_gb),
                    amount: charge,
                    quantity: overage_gb,
                    unit: "GB".to_string(),
                });
            }
        }

        charges
    }

    /// Generate invoice for usage
    pub async fn generate_invoice(
        &self,
        service_id: u64,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Option<u64>> {
        let summary = self.get_usage_summary(service_id, start, end).await?;
        let charges = self.calculate_charges(&summary);

        if charges.is_empty() {
            info!("No billable charges for service {}", service_id);
            return Ok(None);
        }

        // Convert charges to invoice items
        let items: Vec<InvoiceItem> = charges
            .iter()
            .map(|c| InvoiceItem::new(&c.description, c.amount))
            .collect();

        let total: f64 = charges.iter().map(|c| c.amount).sum();
        info!(
            "Generating invoice for service {} - {} items totaling ${:.2}",
            service_id,
            items.len(),
            total
        );

        let invoice_id = self.api.create_invoice(summary.client_id, items, None).await?;

        // Update last billed timestamp
        {
            let mut last_billed = self.last_billed.write().await;
            last_billed.insert(service_id, end);
        }

        Ok(Some(invoice_id))
    }

    /// Process billing for all services
    pub async fn process_billing(&self) -> Result<BillingReport> {
        let records = self.usage_records.read().await;
        let last_billed = self.last_billed.read().await;
        let now = Utc::now();

        let mut report = BillingReport {
            timestamp: now,
            services_processed: 0,
            invoices_generated: 0,
            total_amount: 0.0,
            errors: Vec::new(),
        };

        for service_id in records.keys() {
            let start = last_billed
                .get(service_id)
                .copied()
                .unwrap_or_else(|| now - chrono::Duration::hours(24));

            match self.generate_invoice(*service_id, start, now).await {
                Ok(Some(invoice_id)) => {
                    report.services_processed += 1;
                    report.invoices_generated += 1;
                    info!("Generated invoice {} for service {}", invoice_id, service_id);
                }
                Ok(None) => {
                    report.services_processed += 1;
                }
                Err(e) => {
                    warn!("Failed to process billing for service {}: {}", service_id, e);
                    report.errors.push(format!("Service {}: {}", service_id, e));
                }
            }
        }

        Ok(report)
    }

    /// Clear old usage records
    pub async fn cleanup_old_records(&self, max_age_days: i64) {
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days);

        let mut records = self.usage_records.write().await;
        for service_records in records.values_mut() {
            service_records.retain(|r| r.timestamp > cutoff);
        }

        info!("Cleaned up usage records older than {} days", max_age_days);
    }
}

/// Individual billing charge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingCharge {
    /// Charge description
    pub description: String,
    /// Amount in currency
    pub amount: f64,
    /// Quantity
    pub quantity: f64,
    /// Unit of measure
    pub unit: String,
}

/// Billing report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingReport {
    /// Report timestamp
    pub timestamp: DateTime<Utc>,
    /// Number of services processed
    pub services_processed: u32,
    /// Number of invoices generated
    pub invoices_generated: u32,
    /// Total amount billed
    pub total_amount: f64,
    /// Errors encountered
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_rates() {
        let rates = BillingRates::default();
        assert!(rates.ram_per_gb_hour > 0.0);
        assert!(rates.free_tier.ram_bytes > 0);
    }

    #[test]
    fn test_calculate_charges_no_overage() {
        let rates = BillingRates::default();
        let api = Arc::new(WhmcsApi::new(crate::WhmcsConfig::default()));
        let manager = BillingManager::new(api, rates);

        let summary = UsageSummary {
            service_id: 1,
            client_id: 1,
            period_start: Utc::now() - chrono::Duration::hours(24),
            period_end: Utc::now(),
            avg_cpu_percent: 50.0,
            peak_cpu_percent: 80.0,
            avg_memory_bytes: 512 * 1024 * 1024, // 512 MB
            peak_memory_bytes: 800 * 1024 * 1024, // 800 MB
            disk_bytes: 5 * 1024 * 1024 * 1024,
            total_bandwidth_bytes: 10 * 1024 * 1024 * 1024,
            peak_connections: 5,
            sample_count: 24,
        };

        let charges = manager.calculate_charges(&summary);
        assert!(charges.is_empty()); // Within free tier
    }

    #[test]
    fn test_calculate_charges_with_overage() {
        let rates = BillingRates::default();
        let api = Arc::new(WhmcsApi::new(crate::WhmcsConfig::default()));
        let manager = BillingManager::new(api, rates);

        let summary = UsageSummary {
            service_id: 1,
            client_id: 1,
            period_start: Utc::now() - chrono::Duration::hours(24),
            period_end: Utc::now(),
            avg_cpu_percent: 150.0,
            peak_cpu_percent: 200.0, // 2 cores - 1 core overage
            avg_memory_bytes: 2 * 1024 * 1024 * 1024, // 2 GB
            peak_memory_bytes: 3 * 1024 * 1024 * 1024, // 3 GB - 2 GB overage
            disk_bytes: 5 * 1024 * 1024 * 1024,
            total_bandwidth_bytes: 200 * 1024 * 1024 * 1024, // 200 GB - 100 GB overage
            peak_connections: 5,
            sample_count: 24,
        };

        let charges = manager.calculate_charges(&summary);
        assert!(!charges.is_empty());
        assert!(charges.iter().any(|c| c.description.contains("RAM")));
        assert!(charges.iter().any(|c| c.description.contains("CPU")));
        assert!(charges.iter().any(|c| c.description.contains("Bandwidth")));
    }
}
