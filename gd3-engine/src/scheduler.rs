use crate::resume::Segment;

const MIN_SEGMENT_SIZE: u64 = 512 * 1024; // 512KB
const MAX_CONNECTIONS: usize = 64;
const GROWTH_THRESHOLD: f64 = 0.05;
const DECLINE_WINDOW: usize = 3;

pub struct SchedulerConfig {
    pub file_size: u64,
    pub supports_range: bool,
    pub probe_throughput: u64,
    pub max_connections: usize,
}

#[derive(Debug, Clone)]
pub struct SegmentAllocation {
    pub id: u32,
    pub start: u64,
    pub end: u64,
}

pub struct Scheduler {
    config: SchedulerConfig,
    next_segment_id: u32,
    throughput_history: Vec<u64>,
    decline_count: usize,
    no_split_ids: Vec<u32>,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            next_segment_id: 0,
            throughput_history: Vec::new(),
            decline_count: 0,
            no_split_ids: Vec::new(),
        }
    }

    /// 计算初始分片分配
    pub fn initial_allocation(&mut self) -> Vec<SegmentAllocation> {
        if !self.config.supports_range {
            let seg = self.alloc_segment(0, self.config.file_size);
            return vec![seg];
        }
        let conn_count = self.compute_initial_connections();
        let segment_size = self.config.file_size / conn_count as u64;
        let mut segments = Vec::with_capacity(conn_count);
        for i in 0..conn_count {
            let start = i as u64 * segment_size;
            let end = if i == conn_count - 1 {
                self.config.file_size
            } else {
                (i as u64 + 1) * segment_size
            };
            segments.push(self.alloc_segment(start, end));
        }
        segments
    }

    fn compute_initial_connections(&self) -> usize {
        let throughput_mbps = self.config.probe_throughput as f64 / (1024.0 * 1024.0);
        let count = if throughput_mbps < 1.0 {
            8
        } else if throughput_mbps <= 5.0 {
            4
        } else {
            2
        };
        count.min(self.config.max_connections).min(MAX_CONNECTIONS)
    }

    /// 根据当前吞吐量评估是否需要调整连接数
    pub fn evaluate(
        &mut self,
        current_throughput: u64,
        active_segments: &[Segment],
        active_count: usize,
    ) -> SchedulerDecision {
        self.throughput_history.push(current_throughput);
        if self.throughput_history.len() >= 2 {
            let prev = self.throughput_history[self.throughput_history.len() - 2];
            let growth_rate = if prev > 0 {
                (current_throughput as f64 - prev as f64) / prev as f64
            } else {
                0.0
            };

            if growth_rate > GROWTH_THRESHOLD
                && active_count < self.config.max_connections
                && active_count < MAX_CONNECTIONS
            {
                self.decline_count = 0;
                if let Some(split) = self.find_largest_splittable(active_segments) {
                    return SchedulerDecision::Split(split);
                }
            } else if growth_rate < 0.0 {
                self.decline_count += 1;
                if self.decline_count >= DECLINE_WINDOW && active_count > 2 {
                    self.decline_count = 0;
                    if let Some(id) = self.find_slowest(active_segments) {
                        self.no_split_ids.push(id);
                        return SchedulerDecision::MarkSlowest(id);
                    }
                }
            } else {
                self.decline_count = 0;
            }
        }
        SchedulerDecision::NoOp
    }

    /// 从最大可分割分片中窃取工作
    #[allow(dead_code)]
    pub fn steal_work(&mut self, active_segments: &[Segment]) -> Option<SegmentAllocation> {
        self.find_largest_splittable(active_segments)
    }

    fn find_largest_splittable(&mut self, segments: &[Segment]) -> Option<SegmentAllocation> {
        let candidate = segments
            .iter()
            .filter(|s| {
                s.status == 1
                    && !self.no_split_ids.contains(&s.id)
                    && (s.end - s.downloaded) > MIN_SEGMENT_SIZE * 2
            })
            .max_by_key(|s| s.end - s.downloaded)?;
        let midpoint = candidate.downloaded + (candidate.end - candidate.downloaded) / 2;
        Some(self.alloc_segment(midpoint, candidate.end))
    }

    fn find_slowest(&self, segments: &[Segment]) -> Option<u32> {
        segments
            .iter()
            .filter(|s| s.status == 1)
            .min_by_key(|s| s.downloaded - s.start)
            .map(|s| s.id)
    }

    fn alloc_segment(&mut self, start: u64, end: u64) -> SegmentAllocation {
        let id = self.next_segment_id;
        self.next_segment_id += 1;
        SegmentAllocation { id, start, end }
    }
}

#[derive(Debug)]
pub enum SchedulerDecision {
    NoOp,
    Split(SegmentAllocation),
    MarkSlowest(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_allocation_no_range() {
        let config = SchedulerConfig {
            file_size: 1024 * 1024,
            supports_range: false,
            probe_throughput: 0,
            max_connections: 8,
        };
        let mut scheduler = Scheduler::new(config);
        let allocs = scheduler.initial_allocation();
        assert_eq!(allocs.len(), 1);
        assert_eq!(allocs[0].start, 0);
        assert_eq!(allocs[0].end, 1024 * 1024);
    }

    #[test]
    fn test_initial_allocation_with_range() {
        let config = SchedulerConfig {
            file_size: 10 * 1024 * 1024,
            supports_range: true,
            probe_throughput: 512 * 1024, // 0.5 MB/s -> 8 connections
            max_connections: 16,
        };
        let mut scheduler = Scheduler::new(config);
        let allocs = scheduler.initial_allocation();
        assert_eq!(allocs.len(), 8);
        assert_eq!(allocs[0].start, 0);
        assert_eq!(allocs[7].end, 10 * 1024 * 1024);
    }

    #[test]
    fn test_evaluate_growth_triggers_split() {
        let config = SchedulerConfig {
            file_size: 100 * 1024 * 1024,
            supports_range: true,
            probe_throughput: 1024 * 1024,
            max_connections: 16,
        };
        let mut scheduler = Scheduler::new(config);

        let segments = vec![
            Segment { id: 0, start: 0, downloaded: 1024 * 1024, end: 50 * 1024 * 1024, status: 1, retries: 0 },
            Segment { id: 1, start: 50 * 1024 * 1024, downloaded: 51 * 1024 * 1024, end: 100 * 1024 * 1024, status: 1, retries: 0 },
        ];

        // 第一次评估，建立基线
        let decision = scheduler.evaluate(1_000_000, &segments, 2);
        assert!(matches!(decision, SchedulerDecision::NoOp));

        // 第二次评估，增长超过阈值
        let decision = scheduler.evaluate(1_100_000, &segments, 2);
        assert!(matches!(decision, SchedulerDecision::Split(_)));
    }

    #[test]
    fn test_steal_work() {
        let config = SchedulerConfig {
            file_size: 100 * 1024 * 1024,
            supports_range: true,
            probe_throughput: 1024 * 1024,
            max_connections: 16,
        };
        let mut scheduler = Scheduler::new(config);

        let segments = vec![
            Segment { id: 0, start: 0, downloaded: 1024 * 1024, end: 50 * 1024 * 1024, status: 1, retries: 0 },
            Segment { id: 1, start: 50 * 1024 * 1024, downloaded: 99 * 1024 * 1024, end: 100 * 1024 * 1024, status: 1, retries: 0 },
        ];

        let stolen = scheduler.steal_work(&segments);
        assert!(stolen.is_some());
        let alloc = stolen.unwrap();
        // 应该从 segment 0 窃取（剩余最大）
        assert!(alloc.start > 1024 * 1024);
        assert_eq!(alloc.end, 50 * 1024 * 1024);
    }
}
