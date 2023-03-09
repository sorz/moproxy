use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead},
};

use flexstr::{FlexStr, IntoSharedStr, SharedStr, ToSharedStr};

use super::parser;

#[derive(Debug, Clone, Default)]
struct CapRequirements(Vec<HashSet<SharedStr>>);

impl CapRequirements {
    fn add_caps<I, S>(&mut self, caps: I)
    where
        S: ToSharedStr,
        I: Iterator<Item = S>,
    {
        self.0.push(caps.map(|c| c.to_shared_str()).collect());
    }

    fn meet(&self, caps: &HashSet<SharedStr>) -> bool {
        self.0
            .iter()
            .all(|req| req.intersection(&caps).next().is_some())
    }
}

#[derive(Default)]
pub struct Router {
    listen_port_caps: HashMap<u16, CapRequirements>,
    sni_caps: HashMap<Box<[SharedStr]>, CapRequirements>,
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
                    .add_caps(caps.into_iter());
            }
            parser::RuleFilter::Sni(parts) => {
                let parts = parts.into_iter().map(FlexStr::into_shared_str).collect();
                self.sni_caps
                    .entry(parts)
                    .or_default()
                    .add_caps(caps.into_iter());
            }
        }
    }

    fn rule_count(&self) -> usize {
        self.listen_port_caps
            .values()
            .chain(self.sni_caps.values())
            .fold(0, |acc, v| acc + v.0.len())
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
    let abc = HashSet::from_iter(["a", "b", "c"].into_iter().map(SharedStr::from));
    let bc = HashSet::from_iter(["b", "c"].into_iter().map(SharedStr::from));
    let c = HashSet::from_iter(["c"].into_iter().map(SharedStr::from));
    assert!(p1.meet(&abc));
    assert!(!p1.meet(&bc));
    assert!(!p1.meet(&c));
    assert!(p2.meet(&abc));
    assert!(p2.meet(&bc));
    assert!(!p2.meet(&c));
}
