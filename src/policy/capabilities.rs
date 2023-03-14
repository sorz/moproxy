use std::mem;

use flexstr::SharedStr;
use serde::Serialize;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Default, Serialize)]
pub struct CapSet(Box<[SharedStr]>);

impl CapSet {
    pub fn new<I>(caps: I) -> Self
    where
        I: Iterator,
        I::Item: Into<SharedStr>,
    {
        let mut caps: Vec<_> = caps.map(|s| s.into()).collect();
        caps.sort();
        Self(caps.into())
    }

    pub fn has_intersection(&self, other: &Self) -> bool {
        let mut a = &self.0[..];
        let mut b = &other.0[..];
        if a.len() < b.len() {
            mem::swap(&mut a, &mut b);
        }
        while !(a.is_empty() || b.is_empty()) {
            match a.binary_search(&b[0]) {
                Ok(_) => return true,
                Err(n) => {
                    a = &a[n..];
                    b = &b[1..];
                }
            }
        }
        false
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub trait CheckAllCapsMeet {
    fn all_meet_by(self, caps: &CapSet) -> bool;
}

impl<'a, T> CheckAllCapsMeet for T
where
    T: IntoIterator<Item = &'a CapSet>,
{
    fn all_meet_by(self, caps: &CapSet) -> bool {
        self.into_iter().all(|req| req.has_intersection(caps))
    }
}

#[test]
fn test_capset_intersection() {
    let abc = CapSet::new(["a", "b", "c"].into_iter());
    let def = CapSet::new(["d", "e", "f"].into_iter());
    let bcg = CapSet::new(["b", "c", "g"].into_iter());
    let aeg = CapSet::new(["a", "e", "g"].into_iter());
    assert!(!abc.has_intersection(&def));
    assert!(!def.has_intersection(&abc));
    assert!(!def.has_intersection(&bcg));
    assert!(!bcg.has_intersection(&def));
    assert!(def.has_intersection(&aeg));
    assert!(aeg.has_intersection(&def));
    assert!(abc.has_intersection(&aeg));
}
