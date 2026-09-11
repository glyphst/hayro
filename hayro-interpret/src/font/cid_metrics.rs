//! Complete, bounded CID metric declarations (PDF 1.7, Table 117 and 9.7.4.3).
use hayro_syntax::object::dict::keys::{DW, DW2, W, W2};
use hayro_syntax::object::{Array, Dict, Number, Object};
use rustc_hash::FxHashMap;

// Annex C bounds CIDs. Also cap total assignments, including overwritten
// entries, and groups (empty arrays must not bypass the work budget).
const MAX_CIDS: usize = 65_536;

pub(super) struct CidMetrics {
    pub dw: f32,
    pub dw2: (f32, f32),
    pub widths: FxHashMap<u32, [f32; 1]>,
    pub widths2: FxHashMap<u32, [f32; 3]>,
}

impl CidMetrics {
    pub(super) fn read(dict: &Dict<'_>, horizontal: bool) -> Option<Self> {
        let dw = if dict.is_null_or_absent(DW) {
            1000.0
        } else {
            dict.get::<Number>(DW)?.as_i64_exact()? as f32
        };
        let widths = optional_widths(dict, W)?;
        // Vertical entries have no effect in horizontal writing mode.
        let (dw2, widths2) = if horizontal {
            ((880.0, -1000.0), FxHashMap::default())
        } else {
            let dw2 = if dict.is_null_or_absent(DW2) {
                [880.0, -1000.0]
            } else {
                let values = dict.get::<[f32; 2]>(DW2)?;
                values
                    .iter()
                    .all(|value| value.is_finite())
                    .then_some(values)?
            };
            ((dw2[0], dw2[1]), optional_widths(dict, W2)?)
        };
        Some(Self {
            dw,
            dw2,
            widths,
            widths2,
        })
    }
}

fn optional_widths<const N: usize>(
    dict: &Dict<'_>,
    key: &[u8],
) -> Option<FxHashMap<u32, [f32; N]>> {
    if dict.is_null_or_absent(key) {
        Some(FxHashMap::default())
    } else {
        read_widths(&dict.get::<Array<'_>>(key)?)
    }
}

// The raw iterator retains malformed slots. A typed iterator alone would make
// an unreadable tail indistinguishable from the end of an array.
fn objects<'a>(array: &Array<'a>) -> impl Iterator<Item = Option<Object<'a>>> {
    let mut resolved = array.iter::<Object<'a>>();
    array
        .raw_iter()
        .map(move |raw| raw.ok().and_then(|_| resolved.next()))
}

fn cid(object: Object<'_>) -> Option<u32> {
    u16::try_from(object.into_number()?.as_i64_exact()?)
        .ok()
        .map(u32::from)
}

fn values<'a, const N: usize>(
    iter: &mut impl Iterator<Item = Option<Object<'a>>>,
) -> Option<[f32; N]> {
    let mut result = [0.0; N];
    for value in &mut result {
        *value = iter.next()??.into_number()?.as_f32();
        if !value.is_finite() {
            return None;
        }
    }
    Some(result)
}

fn read_widths<const N: usize>(array: &Array<'_>) -> Option<FxHashMap<u32, [f32; N]>> {
    let mut result = FxHashMap::default();
    let mut iter = objects(array);
    let mut assignments = 0_usize;
    let mut groups = 0_usize;
    while let Some(first) = iter.next() {
        groups += 1;
        if groups > MAX_CIDS {
            return None;
        }
        let first = cid(first?)?;
        match iter.next()?? {
            Object::Array(array) => {
                let mut entries = objects(&array).peekable();
                let mut index = first;
                while entries.peek().is_some() {
                    if index >= MAX_CIDS as u32 || assignments == MAX_CIDS {
                        return None;
                    }
                    result.insert(index, values(&mut entries)?);
                    assignments += 1;
                    index += 1;
                }
            }
            last => {
                let last = cid(last)?;
                let count = last.checked_sub(first)? as usize + 1;
                assignments = assignments.checked_add(count)?;
                if assignments > MAX_CIDS {
                    return None;
                }
                let value = values(&mut iter)?;
                for index in first..=last {
                    result.insert(index, value);
                }
            }
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::object::FromBytes;

    fn read(input: &str, horizontal: bool) -> Option<CidMetrics> {
        CidMetrics::read(&Dict::from_bytes(input.as_bytes()).unwrap(), horizontal)
    }

    #[test]
    fn cid_metrics_defaults_and_inactive_vertical_entries() {
        for input in ["<< >>", "<< /DW null /W null /DW2 null /W2 null >>"] {
            let metrics = read(input, false).unwrap();
            assert_eq!(metrics.dw, 1000.0);
            assert_eq!(metrics.dw2, (880.0, -1000.0));
            assert!(metrics.widths.is_empty() && metrics.widths2.is_empty());
        }
        assert!(read("<< /W [] /DW2 /Bad /W2 [1 [null]] >>", true).is_some());
    }

    #[test]
    fn cid_metrics_mixed_ranges_arrays_and_boundaries() {
        let metrics = read("<< /DW -10 /DW2 [912.5 -1234.25] /W [65535 [1.25] 1 [600 800] 7 9 -50 2 [0]] /W2 [1 [-900 300 700] 2 3 -1100.5 450.25 850 65535 [-1 0 1]] >>", false).unwrap();
        assert_eq!(metrics.dw, -10.0);
        assert_eq!(metrics.dw2, (912.5, -1234.25));
        for (cid, width) in [(1, 600.0), (2, 0.0), (7, -50.0), (9, -50.0), (65535, 1.25)] {
            assert_eq!(metrics.widths[&cid], [width]);
        }
        assert_eq!(metrics.widths2[&1], [-900.0, 300.0, 700.0]);
        assert_eq!(metrics.widths2[&3], [-1100.5, 450.25, 850.0]);
        assert_eq!(metrics.widths2[&65535], [-1.0, 0.0, 1.0]);
    }

    #[test]
    fn cid_metrics_reject_failed_defaults_and_incomplete_arrays() {
        for entry in [
            "/DW /Bad",
            "/DW 1000.0",
            "/DW 1.5",
            "/W /Bad",
            "/W [1 [500 /Bad 600]]",
            "/W [1 [500] /Bad]",
            "/W [1 [500] 2]",
            "/W [1 2]",
            "/W [1 2 null]",
            "/W [1.0 [500]]",
            "/W [1 2.0 500]",
            "/W [-1 [500]]",
            "/W [2 1 500]",
            "/W [65536 [500]]",
            "/W [65535 [500 600]]",
            "/W [0 4294967295 500]",
            "/DW2 [880]",
            "/DW2 [880 -1000 null]",
            "/DW2 [880 /Bad]",
            "/W2 [1 [-900 300]]",
            "/W2 [1 [-900 300 700 null]]",
            "/W2 [1 2 -900 300]",
            "/W2 [1 [-900 300 700] /Bad]",
        ] {
            assert!(read(&format!("<< {entry} >>"), false).is_none(), "{entry}");
        }
    }

    #[test]
    fn cid_metrics_bound_expansion_and_repeated_work() {
        let metrics = read("<< /W [0 65535 500] >>", true).unwrap();
        assert_eq!(metrics.widths.len(), MAX_CIDS);
        assert_eq!(metrics.widths[&65535], [500.0]);
        assert!(read("<< /W [0 65535 500 0 [600]] >>", true).is_none());
        assert!(
            read(
                &format!("<< /W [{}] >>", "0 [] ".repeat(MAX_CIDS + 1)),
                true
            )
            .is_none()
        );
    }
}
