pub mod capabilities;
pub mod parser;

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    hash::Hash,
    io::{self, BufRead, BufReader},
    path::Path,
};

use flexstr::{SharedStr, ToSharedStr};

use capabilities::CapSet;
use tracing::info;

#[derive(Default)]
struct RuleSet<K: Eq + Hash>(HashMap<K, HashSet<CapSet>>);

type ListenPortRuleSet = RuleSet<u16>;
type DstDomainRuleSet = RuleSet<SharedStr>;

impl<K: Eq + Hash> RuleSet<K> {
    fn add(&mut self, key: K, caps: CapSet) {
        // TODO: warning duplicated rules
        self.0.entry(key).or_default().insert(caps);
    }

    fn get<'a>(&'a self, key: &'a K) -> impl Iterator<Item = &'a CapSet> {
        self.0.get(key).into_iter().flatten()
    }
}

impl DstDomainRuleSet {
    fn get_recursive<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a CapSet> {
        let mut skip = 0usize;
        name.split_terminator('.')
            .map(move |part| {
                let key = &name[skip..];
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
    dst_domain_ruleset: DstDomainRuleSet,
}

impl Policy {
    pub fn load<R: BufRead>(read: R) -> io::Result<Self> {
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

    pub fn load_from_file<T: AsRef<Path>>(path: T) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let this = Self::load(reader)?;
        info!("policy: {} rule(s) loaded", this.rule_count());
        Ok(this)
    }

    fn add_rule(&mut self, rule: parser::Rule) {
        let parser::Rule { filter, action } = rule;
        let parser::RuleAction::Require(caps) = action;
        match filter {
            parser::RuleFilter::ListenPort(port) => {
                self.listen_port_ruleset.add(port, caps);
            }
            parser::RuleFilter::Sni(parts) => {
                self.dst_domain_ruleset.add(parts.to_shared_str(), caps);
            }
        }
    }

    pub fn rule_count(&self) -> usize {
        self.listen_port_ruleset
            .0
            .values()
            .chain(self.dst_domain_ruleset.0.values())
            .fold(0, |acc, v| acc + v.len())
    }

    pub fn matches(
        &self,
        listen_port: Option<u16>,
        dst_domain: Option<SharedStr>,
    ) -> HashSet<CapSet> {
        let mut rules = HashSet::new();
        if let Some(port) = listen_port {
            rules.extend(self.listen_port_ruleset.get(&port).cloned());
        }
        if let Some(name) = dst_domain {
            rules.extend(self.dst_domain_ruleset.get_recursive(&name).cloned());
        }
        rules
    }
}

#[test]
fn test_router_listen_port() {
    use capabilities::CheckAllCapsMeet;

    let rules = "
        listen-port 1 require a
        listen-port 2 require b
        listen-port 2 require c or d
    ";
    let router = Policy::load(rules.as_bytes()).unwrap();
    assert_eq!(3, router.rule_count());
    let p1 = router.matches(Some(1), None);
    let p2 = router.matches(Some(2), None);
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
fn test_router_get_domain_caps_requirements() {
    let router = Policy::load(
        "
        dst domain . require root
        dst domain com. require com
        dst domain example.com require example
    "
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        3,
        router
            .dst_domain_ruleset
            .get_recursive("test.example.com")
            .count()
    );
    assert_eq!(
        3,
        router
            .dst_domain_ruleset
            .get_recursive("example.com")
            .count()
    );
    assert_eq!(2, router.dst_domain_ruleset.get_recursive("com").count());
    assert_eq!(1, router.dst_domain_ruleset.get_recursive("net").count());
}
