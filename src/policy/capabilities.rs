use std::{fmt::Display, mem};

use flexstr::SharedStr;
use serde::Serialize;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Default, Serialize, PartialOrd, Ord)]
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

impl Display for CapSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.len() > 1 {
            write!(f, "(")?;
        }
        match self.0.first() {
            Some(cap) => write!(f, "{}", cap),
            None => write!(f, "(EMPTY)"),
        }?;
        for cap in self.0.iter().skip(1) {
            write!(f, " OR {}", cap)?;
        }
        if self.0.len() > 1 {
            write!(f, ")")?;
        }
        Ok(())
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

#[test]
fn test_capset_display() {
    assert_eq!("(EMPTY)", CapSet::default().to_string());
    assert_eq!("a", CapSet::new(["a"].into_iter()).to_string());
    assert_eq!("(a OR b)", CapSet::new(["a", "b"].into_iter()).to_string());
}
