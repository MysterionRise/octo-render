//! Commands: the only way the outside world changes a running simulation. They are
//! queued, applied in order at the start of a tick, and logged with that tick, so a
//! seed plus the log replays a run exactly.

use crate::element::ElementId;
use crate::fixed::{Mass, Temp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Overwrite one cell.
    SetCell {
        x: usize,
        y: usize,
        element: ElementId,
        mass: Mass,
        temp: Temp,
    },
    /// Overwrite the half-open rectangle `[x0, x1) × [y0, y1)`.
    FillRect {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        element: ElementId,
        mass: Mass,
        temp: Temp,
    },
    /// Order the solid cell dug out.
    Dig {
        x: usize,
        y: usize,
    },
    /// Order a tile built from debris at the cell.
    Build {
        x: usize,
        y: usize,
    },
    CancelOrders {
        x: usize,
        y: usize,
    },
    SpawnColonist {
        x: usize,
        y: usize,
    },
    SetPriority {
        x: usize,
        y: usize,
        priority: u8,
    },
    /// Place building `def` with its bottom-left cell at `(x, y)`; ignored if invalid.
    Place {
        def: u16,
        x: usize,
        y: usize,
    },
    /// Remove a planned building at the cell, or order a built one taken apart.
    Deconstruct {
        x: usize,
        y: usize,
    },
    /// Flip the enabled switch of the building at the cell.
    Toggle {
        x: usize,
        y: usize,
    },
}

/// A command with the tick it was applied at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub tick: u64,
    pub command: Command,
}

const TAG_SET: u8 = 1;
const TAG_FILL: u8 = 2;
const TAG_DIG: u8 = 3;
const TAG_BUILD: u8 = 4;
const TAG_CANCEL: u8 = 5;
const TAG_SPAWN: u8 = 6;
const TAG_PRIORITY: u8 = 7;
const TAG_PLACE: u8 = 8;
const TAG_DECONSTRUCT: u8 = 9;
const TAG_TOGGLE: u8 = 10;

fn put_usize(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&(v as u64).to_le_bytes());
}

/// Serialises a log as little-endian records.
pub fn encode_log(log: &[LogEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(log.len() * 48);
    out.extend_from_slice(&(log.len() as u64).to_le_bytes());
    for entry in log {
        out.extend_from_slice(&entry.tick.to_le_bytes());
        match entry.command {
            Command::SetCell {
                x,
                y,
                element,
                mass,
                temp,
            } => {
                out.push(TAG_SET);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
                out.extend_from_slice(&element.0.to_le_bytes());
                out.extend_from_slice(&mass.0.to_le_bytes());
                out.extend_from_slice(&temp.0.to_le_bytes());
            }
            Command::FillRect {
                x0,
                y0,
                x1,
                y1,
                element,
                mass,
                temp,
            } => {
                out.push(TAG_FILL);
                for v in [x0, y0, x1, y1] {
                    put_usize(&mut out, v);
                }
                out.extend_from_slice(&element.0.to_le_bytes());
                out.extend_from_slice(&mass.0.to_le_bytes());
                out.extend_from_slice(&temp.0.to_le_bytes());
            }
            Command::Dig { x, y } => {
                out.push(TAG_DIG);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::Build { x, y } => {
                out.push(TAG_BUILD);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::CancelOrders { x, y } => {
                out.push(TAG_CANCEL);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::SpawnColonist { x, y } => {
                out.push(TAG_SPAWN);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::SetPriority { x, y, priority } => {
                out.push(TAG_PRIORITY);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
                out.push(priority);
            }
            Command::Place { def, x, y } => {
                out.push(TAG_PLACE);
                put_usize(&mut out, def as usize);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::Deconstruct { x, y } => {
                out.push(TAG_DECONSTRUCT);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
            Command::Toggle { x, y } => {
                out.push(TAG_TOGGLE);
                put_usize(&mut out, x);
                put_usize(&mut out, y);
            }
        }
    }
    out
}

/// Parses [`encode_log`] output; `None` if the bytes are malformed.
pub fn decode_log(bytes: &[u8]) -> Option<Vec<LogEntry>> {
    let mut pos = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let s = bytes.get(pos..pos.checked_add(n)?)?;
        pos += n;
        Some(s)
    };
    let count = u64::from_le_bytes(take(8)?.try_into().ok()?);
    let mut log = Vec::new();
    for _ in 0..count {
        let tick = u64::from_le_bytes(take(8)?.try_into().ok()?);
        let tag = take(1)?[0];
        let mut usize_field = || -> Option<usize> {
            usize::try_from(u64::from_le_bytes(take(8)?.try_into().ok()?)).ok()
        };
        let command = match tag {
            TAG_SET => {
                let x = usize_field()?;
                let y = usize_field()?;
                let element = ElementId(u16::from_le_bytes(take(2)?.try_into().ok()?));
                let mass = Mass(u32::from_le_bytes(take(4)?.try_into().ok()?));
                let temp = Temp(i32::from_le_bytes(take(4)?.try_into().ok()?));
                Command::SetCell {
                    x,
                    y,
                    element,
                    mass,
                    temp,
                }
            }
            TAG_FILL => {
                let x0 = usize_field()?;
                let y0 = usize_field()?;
                let x1 = usize_field()?;
                let y1 = usize_field()?;
                let element = ElementId(u16::from_le_bytes(take(2)?.try_into().ok()?));
                let mass = Mass(u32::from_le_bytes(take(4)?.try_into().ok()?));
                let temp = Temp(i32::from_le_bytes(take(4)?.try_into().ok()?));
                Command::FillRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    element,
                    mass,
                    temp,
                }
            }
            TAG_DIG | TAG_BUILD | TAG_CANCEL | TAG_SPAWN | TAG_DECONSTRUCT | TAG_TOGGLE => {
                let x = usize_field()?;
                let y = usize_field()?;
                match tag {
                    TAG_DIG => Command::Dig { x, y },
                    TAG_BUILD => Command::Build { x, y },
                    TAG_CANCEL => Command::CancelOrders { x, y },
                    TAG_SPAWN => Command::SpawnColonist { x, y },
                    TAG_DECONSTRUCT => Command::Deconstruct { x, y },
                    _ => Command::Toggle { x, y },
                }
            }
            TAG_PLACE => {
                let def = u16::try_from(usize_field()?).ok()?;
                let x = usize_field()?;
                let y = usize_field()?;
                Command::Place { def, x, y }
            }
            TAG_PRIORITY => {
                let x = usize_field()?;
                let y = usize_field()?;
                let priority = take(1)?[0];
                Command::SetPriority { x, y, priority }
            }
            _ => return None,
        };
        log.push(LogEntry { tick, command });
    }
    Some(log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::builtin;

    #[test]
    fn log_round_trips_and_rejects_garbage() {
        let log = vec![
            LogEntry {
                tick: 3,
                command: Command::SetCell {
                    x: 4,
                    y: 5,
                    element: builtin::WATER,
                    mass: Mass::from_kg(7),
                    temp: Temp::from_kelvin(280),
                },
            },
            LogEntry {
                tick: 9,
                command: Command::FillRect {
                    x0: 1,
                    y0: 2,
                    x1: 3,
                    y1: 4,
                    element: builtin::OXYGEN,
                    mass: Mass::from_grams(500),
                    temp: Temp::from_kelvin(300),
                },
            },
        ];
        let log = [
            log,
            vec![
                LogEntry {
                    tick: 10,
                    command: Command::Dig { x: 1, y: 2 },
                },
                LogEntry {
                    tick: 11,
                    command: Command::Build { x: 3, y: 4 },
                },
                LogEntry {
                    tick: 12,
                    command: Command::CancelOrders { x: 5, y: 6 },
                },
                LogEntry {
                    tick: 20,
                    command: Command::Place { def: 4, x: 8, y: 9 },
                },
                LogEntry {
                    tick: 21,
                    command: Command::Deconstruct { x: 8, y: 9 },
                },
                LogEntry {
                    tick: 22,
                    command: Command::Toggle { x: 8, y: 10 },
                },
                LogEntry {
                    tick: 13,
                    command: Command::SpawnColonist { x: 7, y: 8 },
                },
                LogEntry {
                    tick: 14,
                    command: Command::SetPriority {
                        x: 9,
                        y: 10,
                        priority: 7,
                    },
                },
            ],
        ]
        .concat();
        let bytes = encode_log(&log);
        assert_eq!(decode_log(&bytes).unwrap(), log);
        assert_eq!(decode_log(&bytes[..bytes.len() - 1]), None);
        let mut bad = bytes.clone();
        bad[16] = 77;
        assert_eq!(decode_log(&bad), None);
    }
}
