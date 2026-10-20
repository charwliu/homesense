//! Wire compression retains an individual receipt range for every admitted port.
use super::{MAX_BATCH_BYTES, PreparedFrame};

pub(super) struct Plan {
    pub bytes: Vec<u8>,
    pub ranges: Vec<(usize, usize)>,
}
impl Plan {
    pub fn new(frames: &[&PreparedFrame]) -> Option<Self> {
        // Preserve the original admission/collection bound, even if compression
        // could make a larger request fit on the wire.
        if frames.is_empty() || frames.iter().map(|f| f.length).sum::<usize>() > MAX_BATCH_BYTES {
            return None;
        }
        let mut plan = Self {
            bytes: vec![],
            ranges: vec![],
        };
        let mut index = 0;
        while index < frames.len() {
            let first = frames[index];
            let group = frames.get(index..index + 8);
            let full = first.module_full.is_some()
                && group.is_some_and(|group| {
                    let mut channels = 0u16;
                    for frame in group {
                        if frame.length != 5
                            || frame.module_full != first.module_full
                            || frame.transport_epoch != first.transport_epoch
                            || frame.context != first.context
                        {
                            return false;
                        }
                        let channel = frame.bytes[2];
                        if !(1..=8).contains(&channel) || channels & (1 << channel) != 0 {
                            return false;
                        }
                        channels |= 1 << channel;
                    }
                    channels == 0x1fe
                });
            let start = plan.bytes.len();
            if full {
                plan.bytes.extend_from_slice(&first.module_full?);
                // A partial five-byte module frame leaves all eight operations
                // partial; none may be reported Written before its final byte.
                plan.ranges
                    .extend(std::iter::repeat_n((start, start + 5), 8));
                index += 8;
            } else {
                plan.bytes.extend_from_slice(&first.bytes[..first.length]);
                plan.ranges.push((start, plan.bytes.len()));
                index += 1;
            }
        }
        Some(plan)
    }
}

/// Actual bounded wire length, after complete eight-port On/Off groups are encoded.
/// No state cache is used and no missing channel is filled in.
pub fn encoded_batch_len(frames: &[&PreparedFrame]) -> Option<usize> {
    Plan::new(frames).map(|plan| plan.bytes.len())
}
