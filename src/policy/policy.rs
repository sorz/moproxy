use std::{
    collections::{HashMap, HashSet},
    io::{self, BufRead},
};

use flexstr::{SharedStr, ToSharedStr};

use super::{capabilities::CapSet, parser};

#[derive(Default)]
struct ListenPortRuleSet(HashMap<u16, HashSet<CapSet>>);

#[derive(Default)]
struct SniRuleSet(HashMap<SharedStr, HashSet<CapSet>>);

impl ListenPortRuleSet {
    fn add(&mut self, port: u16, caps: CapSet) {
        // TODO: warning duplicated rules
        self.0.entry(port).or_default().insert(caps);
    }

    fn get<'a>(&'a self, port: &'a u16) -> impl Iterator<Item = &'a CapSet> {
        self.0.get(&port).into_iter().flatten()
    }
}

impl SniRuleSet {
    fn add(&mut self, sni: SharedStr, caps: CapSet) {
        // TODO: warning duplicated rules
        self.0.entry(sni).or_default().insert(caps);
    }

    fn get<'a>(&'a self, sni: &'a str) -> impl Iterator<Item = &'a CapSet> {
        let mut skip = 0usize;
        sni.split_terminator('.')
            .map(move |part| {
                let key = &sni[skip..];
                skip += part.len() + 1;
                key
            })
            .chain(["."])
            .filter_map(|key| self.0.get(key))
            .flatten()
    }
}

#[derive(Default)]
pub struct Policy {
    listen_port_ruleset: ListenPortRuleSet,
    sni_ruleset: SniRuleSet,
}

impl Policy {
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
                self.listen_port_ruleset.add(port, caps);
            }
            parser::RuleFilter::Sni(parts) => {
                self.sni_ruleset.add(parts.to_shared_str(), caps);
            }
        }
    }

    fn rule_count(&self) -> usize {
        self.listen_port_ruleset
            .0
            .values()
            .chain(self.sni_ruleset.0.values())
            .fold(0, |acc, v| acc + v.len())
    }

    fn get_rules(&self, listen_port: Option<u16>, sni: Option<SharedStr>) -> HashSet<CapSet> {
        let mut rules = HashSet::new();
        if let Some(port) = listen_port {
            rules.extend(self.listen_port_ruleset.get(&port).cloned());
        }
        if let Some(sni) = sni {
            rules.extend(self.sni_ruleset.get(&sni).cloned());
        }
        rules
    }
}

#[test]
fn test_router_listen_port() {
    use super::capabilities::CheckAllCapsMeet;

    let rules = "
        listen-port 1 require a
        listen-port 2 require b
        listen-port 2 require c or d
    ";
    let router = Policy::from_file(rules.as_bytes()).unwrap();
    assert_eq!(3, router.rule_count());
    let p1 = router.get_rules(Some(1), None);
    let p2 = router.get_rules(Some(2), None);
    let abc = CapSet::new(["a", "b", "c"].into_iter());
    let bc = CapSet::new(["b", "c"].into_iter());
    let c = CapSet::new(["c"].into_iter());
    assert!(p1.all_meet_by(&abc));
    assert!(!p1.all_meet_by(&bc));
    assert!(!p1.all_meet_by(&c));
    assert!(p2.all_meet_by(&abc));
    assert!(p2.all_meet_by(&bc));
    assert!(!p2.all_meet_by(&c));
}

#[test]
fn test_router_get_sni_caps_requirements() {
    let router = Policy::from_file(
        "
        sni . require root
        sni com. require com
        sni example.com require example
    "
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(3, router.sni_ruleset.get("test.example.com").count());
    assert_eq!(3, router.sni_ruleset.get("example.com").count());
    assert_eq!(2, router.sni_ruleset.get("com").count());
    assert_eq!(1, router.sni_ruleset.get("net").count());
}
