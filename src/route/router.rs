use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead},
    ops::{Add, AddAssign},
};

use flexstr::{SharedStr, ToSharedStr};

use super::{capabilities::CapSet, parser};

#[derive(Debug, Clone, Default)]
struct CapRequirements(HashSet<CapSet>);

impl Add for CapRequirements {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.into_iter().chain(rhs.0.into_iter()).collect())
    }
}

impl AddAssign for CapRequirements {
    fn add_assign(&mut self, rhs: Self) {
        self.0.extend(rhs.0.into_iter())
    }
}

impl CapRequirements {
    fn add_caps(&mut self, caps: CapSet) {
        self.0.insert(caps);
    }

    fn meet(&self, caps: &CapSet) -> bool {
        self.0.iter().all(|req| req.has_intersection(caps))
    }
}

#[derive(Default)]
pub struct Router {
    listen_port_caps: HashMap<u16, CapRequirements>,
    sni_caps: HashMap<SharedStr, CapRequirements>,
}

impl Router {
    pub fn from_file<R: BufRead>(read: R) -> io::Result<Self> {
        let mut router: Self = Default::default();
        for line in read.lines() {
            match parser::line_no_ending(&line?) {
                Ok((_, None)) => (),
                Ok((_, Some(rule))) => router.add_rule(rule),
                Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err.to_owned())),
            }
        }
        Ok(router)
    }

    fn add_rule(&mut self, rule: parser::Rule) {
        let parser::Rule { filter, action } = rule;
        let caps = match action {
            parser::RuleAction::Require(caps) => caps,
        };
        match filter {
            parser::RuleFilter::ListenPort(port) => {
                self.listen_port_caps
                    .entry(port)
                    .or_default()
                    .add_caps(caps);
            }
            parser::RuleFilter::Sni(parts) => {
                let parts = parts.to_shared_str();
                self.sni_caps.entry(parts).or_default().add_caps(caps);
            }
        }
    }

    fn rule_count(&self) -> usize {
        self.listen_port_caps
            .values()
            .chain(self.sni_caps.values())
            .fold(0, |acc, v| acc + v.0.len())
    }

    fn get_sni_caps(&self, sni: &str) -> CapRequirements {
        let caps = CapRequirements::default();
        unimplemented!()
    }
}

#[test]
fn test_router_listen_port() {
    let rules = "
        listen-port 1 require a
        listen-port 2 require b
        listen-port 2 require c or d
    ";
    let router = Router::from_file(rules.as_bytes()).unwrap();
    assert_eq!(3, router.rule_count());
    let p1 = router.listen_port_caps.get(&1).unwrap();
    let p2 = router.listen_port_caps.get(&2).unwrap();
    let abc = CapSet::new(["a", "b", "c"].into_iter());
    let bc = CapSet::new(["b", "c"].into_iter());
    let c = CapSet::new(["c"].into_iter());
    assert!(p1.meet(&abc));
    assert!(!p1.meet(&bc));
    assert!(!p1.meet(&c));
    assert!(p2.meet(&abc));
    assert!(p2.meet(&bc));
    assert!(!p2.meet(&c));
}
