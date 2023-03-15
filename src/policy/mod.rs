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

#[derive(Debug)]
pub enum Action {
    Require(HashSet<CapSet>),
    Direct,
}

impl Default for Action {
    fn default() -> Self {
        Self::Require(Default::default())
    }
}

impl Action {
    fn add_require(&mut self, caps: CapSet) {
        match self {
            Self::Direct => false,
            Self::Require(set) => set.insert(caps),
        };
    }

    fn set_direct(&mut self) {
        *self = Self::Direct
    }

    fn len(&self) -> usize {
        match self {
            Self::Direct => 1,
            Self::Require(set) => set.len(),
        }
    }

    fn extend(&mut self, other: &Self) {
        match other {
            Self::Direct => self.set_direct(),
            Self::Require(new_caps) => {
                if let Self::Require(caps) = self {
                    caps.extend(new_caps.iter().cloned())
                }
            }
        }
    }
}

#[derive(Default)]
struct RuleSet<K: Eq + Hash>(HashMap<K, Action>);

type ListenPortRuleSet = RuleSet<u16>;
type DstDomainRuleSet = RuleSet<SharedStr>;

impl<K: Eq + Hash> RuleSet<K> {
    fn add(&mut self, key: K, action: parser::RuleAction) {
        // TODO: warning duplicated rules
        let value = self.0.entry(key).or_default();
        match action {
            parser::RuleAction::Require(caps) => value.add_require(caps),
            parser::RuleAction::Direct => value.set_direct(),
        }
    }

    fn get<'a>(&'a self, key: &'a K) -> impl Iterator<Item = &'a Action> {
        self.0.get(key).into_iter()
    }
}

impl DstDomainRuleSet {
    fn get_recursive<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Action> {
        let mut skip = 0usize;
        name.split_terminator('.')
            .map(move |part| {
                let key = &name[skip..];
                skip += part.len() + 1;
                key
            })
            .chain(["."])
            .filter_map(|key| self.0.get(key))
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
        match filter {
            parser::RuleFilter::ListenPort(port) => {
                self.listen_port_ruleset.add(port, action);
            }
            parser::RuleFilter::Sni(parts) => {
                self.dst_domain_ruleset.add(parts.to_shared_str(), action);
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

    pub fn matches(&self, listen_port: Option<u16>, dst_domain: Option<SharedStr>) -> Action {
        let mut action: Action = Default::default();
        if let Some(port) = listen_port {
            self.listen_port_ruleset
                .get(&port)
                .for_each(|a| action.extend(a))
        }
        if let Some(name) = dst_domain {
            self.dst_domain_ruleset
                .get_recursive(&name)
                .for_each(|a| action.extend(a));
        }
        action
    }
}

#[test]
fn test_policy_listen_port() {
    use capabilities::CheckAllCapsMeet;

    let rules = "
        listen-port 1 require a
        listen-port 2 require b
        listen-port 2 require c or d
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    assert_eq!(3, policy.rule_count());
    let p1 = match policy.matches(Some(1), None) {
        Action::Require(a) => a,
        _ => panic!(),
    };
    let p2 = match policy.matches(Some(2), None) {
        Action::Require(a) => a,
        _ => panic!(),
    };
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
fn test_policy_get_domain_caps_requirements() {
    let policy = Policy::load(
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
        policy
            .dst_domain_ruleset
            .get_recursive("test.example.com")
            .count()
    );
    assert_eq!(
        3,
        policy
            .dst_domain_ruleset
            .get_recursive("example.com")
            .count()
    );
    assert_eq!(2, policy.dst_domain_ruleset.get_recursive("com").count());
    assert_eq!(1, policy.dst_domain_ruleset.get_recursive("net").count());
}

#[test]
fn test_policy_action_direct() {
    let rules = "
        listen-port 1 require a
        listen-port 1 direct
        dst domain test require c
    ";
    let policy = Policy::load(rules.as_bytes()).unwrap();
    let direct = policy.matches(Some(1), Some("test".into()));
    let require = policy.matches(Some(2), Some("test".into()));
    assert!(matches!(direct, Action::Direct));
    assert!(matches!(require, Action::Require(_)));
}
