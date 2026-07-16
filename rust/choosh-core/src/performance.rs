//! Deterministic terminal performance evidence aggregation.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceIdentity {
    pub tier: String,
    pub renderer: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerformanceBudgets {
    pub max_frame_p95_micros: u64,
    pub max_input_p95_micros: u64,
    pub min_output_bytes_per_second: u64,
    pub max_memory_bytes: u64,
    pub require_gpu_recovery: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerformanceLimits {
    pub max_samples_per_metric: usize,
    pub max_identity_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Metric {
    FrameMicros,
    InputLatencyMicros,
    OutputBytesPerSecond,
    MemoryBytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuRecovery {
    NotExercised,
    Recovered,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetricEvidence {
    pub samples: usize,
    pub minimum: u64,
    pub maximum: u64,
    pub p50: u64,
    pub p95: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetCheck {
    pub metric: &'static str,
    pub measured: Option<u64>,
    pub budget: Option<u64>,
    pub passed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerformanceEvidence {
    pub device: DeviceIdentity,
    pub frame: Option<MetricEvidence>,
    pub input: Option<MetricEvidence>,
    pub output: Option<MetricEvidence>,
    pub memory: Option<MetricEvidence>,
    pub gpu_recovery: GpuRecovery,
    pub checks: Vec<BudgetCheck>,
    pub passed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PerformanceError {
    InvalidLimits,
    InvalidBudgets,
    InvalidIdentity,
    SampleCapacity,
    MissingSamples,
    PercentileOverflow,
}

#[derive(Debug)]
pub struct PerformanceAggregator {
    device: DeviceIdentity,
    budgets: PerformanceBudgets,
    limits: PerformanceLimits,
    frame: Vec<u64>,
    input: Vec<u64>,
    output: Vec<u64>,
    memory: Vec<u64>,
    gpu_recovery: GpuRecovery,
}

impl PerformanceAggregator {
    /// Creates an evidence aggregator with explicit device and renderer identity.
    ///
    /// # Errors
    ///
    /// Rejects zero bounds/budgets and empty or oversized identity fields.
    pub fn new(
        device: DeviceIdentity,
        budgets: PerformanceBudgets,
        limits: PerformanceLimits,
    ) -> Result<Self, PerformanceError> {
        if limits.max_samples_per_metric == 0 || limits.max_identity_bytes == 0 {
            return Err(PerformanceError::InvalidLimits);
        }
        if budgets.max_frame_p95_micros == 0
            || budgets.max_input_p95_micros == 0
            || budgets.min_output_bytes_per_second == 0
            || budgets.max_memory_bytes == 0
        {
            return Err(PerformanceError::InvalidBudgets);
        }
        if device.tier.is_empty()
            || device.renderer.is_empty()
            || device.tier.len() > limits.max_identity_bytes
            || device.renderer.len() > limits.max_identity_bytes
        {
            return Err(PerformanceError::InvalidIdentity);
        }
        Ok(Self {
            device,
            budgets,
            limits,
            frame: Vec::new(),
            input: Vec::new(),
            output: Vec::new(),
            memory: Vec::new(),
            gpu_recovery: GpuRecovery::NotExercised,
        })
    }

    /// Adds one injected measurement without consulting a wall clock.
    ///
    /// # Errors
    ///
    /// Rejects a metric once its configured sample capacity is reached.
    pub fn record(&mut self, metric: Metric, value: u64) -> Result<(), PerformanceError> {
        let samples = match metric {
            Metric::FrameMicros => &mut self.frame,
            Metric::InputLatencyMicros => &mut self.input,
            Metric::OutputBytesPerSecond => &mut self.output,
            Metric::MemoryBytes => &mut self.memory,
        };
        if samples.len() == self.limits.max_samples_per_metric {
            return Err(PerformanceError::SampleCapacity);
        }
        samples.push(value);
        Ok(())
    }

    pub const fn record_gpu_recovery(&mut self, outcome: GpuRecovery) {
        self.gpu_recovery = outcome;
    }

    /// Produces immutable machine-verifiable evidence and budget checks.
    ///
    /// # Errors
    ///
    /// Rejects missing metric samples or checked percentile-index overflow.
    pub fn finish(self) -> Result<PerformanceEvidence, PerformanceError> {
        let frame = summarize(&self.frame)?;
        let input = summarize(&self.input)?;
        let output = summarize(&self.output)?;
        let memory = summarize(&self.memory)?;
        let mut checks = vec![
            BudgetCheck {
                metric: "frame_p95_micros",
                measured: Some(frame.p95),
                budget: Some(self.budgets.max_frame_p95_micros),
                passed: frame.p95 <= self.budgets.max_frame_p95_micros,
            },
            BudgetCheck {
                metric: "input_p95_micros",
                measured: Some(input.p95),
                budget: Some(self.budgets.max_input_p95_micros),
                passed: input.p95 <= self.budgets.max_input_p95_micros,
            },
            BudgetCheck {
                metric: "output_min_bytes_per_second",
                measured: Some(output.minimum),
                budget: Some(self.budgets.min_output_bytes_per_second),
                passed: output.minimum >= self.budgets.min_output_bytes_per_second,
            },
            BudgetCheck {
                metric: "memory_max_bytes",
                measured: Some(memory.maximum),
                budget: Some(self.budgets.max_memory_bytes),
                passed: memory.maximum <= self.budgets.max_memory_bytes,
            },
        ];
        if self.budgets.require_gpu_recovery {
            checks.push(BudgetCheck {
                metric: "gpu_recovery",
                measured: None,
                budget: None,
                passed: self.gpu_recovery == GpuRecovery::Recovered,
            });
        }
        let passed = checks.iter().all(|check| check.passed);
        Ok(PerformanceEvidence {
            device: self.device,
            frame: Some(frame),
            input: Some(input),
            output: Some(output),
            memory: Some(memory),
            gpu_recovery: self.gpu_recovery,
            checks,
            passed,
        })
    }
}

fn summarize(samples: &[u64]) -> Result<MetricEvidence, PerformanceError> {
    if samples.is_empty() {
        return Err(PerformanceError::MissingSamples);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    Ok(MetricEvidence {
        samples: sorted.len(),
        minimum: sorted[0],
        maximum: sorted[sorted.len() - 1],
        p50: nearest_rank(&sorted, 50)?,
        p95: nearest_rank(&sorted, 95)?,
    })
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> Result<u64, PerformanceError> {
    let numerator = sorted
        .len()
        .checked_mul(percentile)
        .and_then(|value| value.checked_add(99))
        .ok_or(PerformanceError::PercentileOverflow)?;
    let rank = numerator / 100;
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .ok_or(PerformanceError::PercentileOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aggregator(capacity: usize) -> PerformanceAggregator {
        PerformanceAggregator::new(
            DeviceIdentity {
                tier: "mid".into(),
                renderer: "wgpu-vulkan".into(),
            },
            PerformanceBudgets {
                max_frame_p95_micros: 20,
                max_input_p95_micros: 10,
                min_output_bytes_per_second: 100,
                max_memory_bytes: 1_000,
                require_gpu_recovery: true,
            },
            PerformanceLimits {
                max_samples_per_metric: capacity,
                max_identity_bytes: 32,
            },
        )
        .unwrap()
    }

    fn record_all(aggregator: &mut PerformanceAggregator) {
        for value in 1..=20 {
            aggregator.record(Metric::FrameMicros, value).unwrap();
            aggregator
                .record(Metric::InputLatencyMicros, value / 2)
                .unwrap();
            aggregator
                .record(Metric::OutputBytesPerSecond, 100 + value)
                .unwrap();
            aggregator.record(Metric::MemoryBytes, 900 + value).unwrap();
        }
    }

    #[test]
    fn nearest_rank_evidence_and_pass_verdict_are_deterministic() {
        let mut aggregator = aggregator(20);
        record_all(&mut aggregator);
        aggregator.record_gpu_recovery(GpuRecovery::Recovered);
        let evidence = aggregator.finish().unwrap();
        assert_eq!(evidence.frame.as_ref().unwrap().p50, 10);
        assert_eq!(evidence.frame.as_ref().unwrap().p95, 19);
        assert!(evidence.passed);
        assert_eq!(evidence.device.renderer, "wgpu-vulkan");
    }

    #[test]
    fn every_failed_budget_is_machine_visible() {
        let mut aggregator = aggregator(1);
        aggregator.record(Metric::FrameMicros, 21).unwrap();
        aggregator.record(Metric::InputLatencyMicros, 11).unwrap();
        aggregator.record(Metric::OutputBytesPerSecond, 99).unwrap();
        aggregator.record(Metric::MemoryBytes, 1_001).unwrap();
        aggregator.record_gpu_recovery(GpuRecovery::Failed);
        let evidence = aggregator.finish().unwrap();
        assert!(!evidence.passed);
        assert_eq!(
            evidence.checks.iter().filter(|check| !check.passed).count(),
            5
        );
    }

    #[test]
    fn sample_capacity_is_per_metric_and_atomic() {
        let mut aggregator = aggregator(1);
        aggregator.record(Metric::FrameMicros, 1).unwrap();
        assert_eq!(
            aggregator.record(Metric::FrameMicros, 2),
            Err(PerformanceError::SampleCapacity)
        );
        assert!(aggregator.record(Metric::InputLatencyMicros, 1).is_ok());
    }

    #[test]
    fn missing_samples_and_invalid_identity_fail_closed() {
        assert_eq!(
            aggregator(1).finish(),
            Err(PerformanceError::MissingSamples)
        );
        assert!(matches!(
            PerformanceAggregator::new(
                DeviceIdentity {
                    tier: String::new(),
                    renderer: "r".into()
                },
                PerformanceBudgets {
                    max_frame_p95_micros: 1,
                    max_input_p95_micros: 1,
                    min_output_bytes_per_second: 1,
                    max_memory_bytes: 1,
                    require_gpu_recovery: false,
                },
                PerformanceLimits {
                    max_samples_per_metric: 1,
                    max_identity_bytes: 1
                }
            ),
            Err(PerformanceError::InvalidIdentity)
        ));
    }

    #[test]
    fn unordered_samples_produce_stable_percentiles() {
        assert_eq!(
            summarize(&[100, 1, 50, 25]).unwrap(),
            MetricEvidence {
                samples: 4,
                minimum: 1,
                maximum: 100,
                p50: 25,
                p95: 100,
            }
        );
    }
}
