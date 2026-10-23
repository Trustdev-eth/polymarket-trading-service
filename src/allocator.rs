//! Pro-rata FAK split across tenants sharing one CLOB touch.

use serde::{Deserialize, Serialize};

pub const HAIRCUT: f64 = 0.5;
pub const MIN_SIZE: f64 = 1.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub tenant_id: String,
    pub requested: f64,
    pub deficit: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FillShare {
    pub tenant_id: String,
    pub size: f64,
    pub requested: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Allocation {
    pub budget: f64,
    pub shares: Vec<FillShare>,
    /// Tenants that wanted a fill this wave but got 0 (thin book).
    pub missed: Vec<String>,
}

/// `budget` is already haircut live size (contracts). `min_size` is venue min (usually 1).
pub fn allocate(budget: f64, min_size: f64, mut cands: Vec<Candidate>) -> Allocation {
    let min_size = min_size.max(MIN_SIZE);
    cands.retain(|c| c.requested + 1e-9 >= min_size);
    if cands.is_empty() || budget + 1e-9 < min_size {
        return Allocation {
            budget: budget.max(0.0),
            shares: Vec::new(),
            missed: cands.into_iter().map(|c| c.tenant_id).collect(),
        };
    }

    cands.sort_by(|a, b| {
        b.deficit
            .cmp(&a.deficit)
            .then_with(|| a.tenant_id.cmp(&b.tenant_id))
    });

    let mut remaining = budget;
    let mut shares = Vec::new();
    let mut missed = Vec::new();

    let slots = (remaining / min_size).floor() as usize;
    if slots < cands.len() {
        for (i, c) in cands.iter().enumerate() {
            if i < slots {
                shares.push(FillShare {
                    tenant_id: c.tenant_id.clone(),
                    size: min_size,
                    requested: c.requested,
                });
                remaining -= min_size;
            } else {
                missed.push(c.tenant_id.clone());
            }
        }
        return Allocation {
            budget,
            shares,
            missed,
        };
    }

    let weight: f64 = cands.iter().map(|c| c.requested.max(min_size)).sum();
    let mut leftover = remaining;
    for c in &cands {
        let raw = remaining * (c.requested.max(min_size) / weight);
        let size = (raw / min_size).floor() * min_size;
        let size = size.min(c.requested).min(leftover);
        if size + 1e-9 >= min_size {
            leftover -= size;
            shares.push(FillShare {
                tenant_id: c.tenant_id.clone(),
                size,
                requested: c.requested,
            });
        } else {
            missed.push(c.tenant_id.clone());
        }
    }

    let mut i = 0;
    while leftover + 1e-9 >= min_size && !cands.is_empty() {
        let c = &cands[i % cands.len()];
        if let Some(s) = shares.iter_mut().find(|s| s.tenant_id == c.tenant_id) {
            if s.size + min_size <= c.requested + 1e-9 {
                s.size += min_size;
                leftover -= min_size;
            }
        } else if let Some(pos) = missed.iter().position(|id| id == &c.tenant_id) {
            missed.remove(pos);
            shares.push(FillShare {
                tenant_id: c.tenant_id.clone(),
                size: min_size,
                requested: c.requested,
            });
            leftover -= min_size;
        }
        i += 1;
        if i > cands.len() * 8 {
            break;
        }
    }

    Allocation {
        budget,
        shares,
        missed,
    }
}

/// Visible touch, haircut 50%, then cap at `max_wave`. Floored to whole contracts.
pub fn wave_budget(touch_size: f64, max_wave: f64) -> f64 {
    let raw = (touch_size.max(0.0) * HAIRCUT).min(max_wave.max(0.0));
    (raw / MIN_SIZE).floor() * MIN_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, req: f64, def: u32) -> Candidate {
        Candidate {
            tenant_id: id.into(),
            requested: req,
            deficit: def,
        }
    }

    #[test]
    fn splits_thick_book_by_requested() {
        let out = allocate(
            20.0,
            1.0,
            vec![c("a", 5.0, 0), c("b", 5.0, 0), c("c", 10.0, 0)],
        );
        assert!(out.missed.is_empty());
        let a = out.shares.iter().find(|s| s.tenant_id == "a").unwrap();
        let big = out.shares.iter().find(|s| s.tenant_id == "c").unwrap();
        assert!((a.size - 5.0).abs() < 1e-9);
        assert!((big.size - 10.0).abs() < 1e-9);
    }

    #[test]
    fn thin_book_prefers_deficit() {
        let out = allocate(
            2.0,
            1.0,
            vec![c("new", 5.0, 0), c("owed", 5.0, 3), c("also", 5.0, 1)],
        );
        assert_eq!(out.shares.len(), 2);
        assert_eq!(out.shares[0].tenant_id, "owed");
        assert_eq!(out.shares[1].tenant_id, "also");
        assert_eq!(out.missed, vec!["new".to_string()]);
    }

    #[test]
    fn haircut_caps_wave() {
        assert!((wave_budget(40.0, 1000.0) - 20.0).abs() < 1e-12);
        assert!((wave_budget(40.0, 10.0) - 10.0).abs() < 1e-12);
    }

    #[test]
    fn paper_fleet_of_twenty_on_thin_book() {
        let cands: Vec<_> = (0..20)
            .map(|i| c(&format!("u{i:02}"), 5.0, if i == 7 { 4 } else { 0 }))
            .collect();
        let out = allocate(wave_budget(10.0, 80.0), 1.0, cands);
        assert_eq!(out.budget, 5.0);
        assert_eq!(out.shares.len(), 5);
        assert_eq!(out.shares[0].tenant_id, "u07");
        assert_eq!(out.missed.len(), 15);
    }

    #[test]
    fn hundred_trialists_share_thick_touch() {
        let cands: Vec<_> = (0..100)
            .map(|i| {
                let test = i < 28;
                c(&format!("t{i:03}"), if test { 2.0 } else { 5.0 }, 0)
            })
            .collect();
        let out = allocate(wave_budget(400.0, 80.0), 1.0, cands);
        assert_eq!(out.budget, 80.0);
        assert!(!out.shares.is_empty());
        let got: f64 = out.shares.iter().map(|s| s.size).sum();
        assert!((got - 80.0).abs() < 1e-9);
        assert_eq!(out.shares.len() + out.missed.len(), 100);
    }
}
