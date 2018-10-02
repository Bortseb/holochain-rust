pub mod actor;
pub mod header;

use actor::Protocol;
use chain::{
    actor::{AskChain, ChainActor},
};
use error::HolochainError;
use hash_table::{sys_entry::ToEntry, HashTable};
use json::ToJson;
use riker::actors::*;
use serde_json;
use hash_table::sys_entry::EntryType;
use cas::content::AddressableContent;
use hash_table::entry::EntryHeader;
use chain::header::ChainHeader;
use hash_table::entry::Entry;

/// Iterator type for pairs in a chain
/// next method may panic if there is an error in the underlying table
#[derive(Clone)]
pub struct ChainIterator {
    table_actor: ActorRef<Protocol>,
    current: Option<ChainHeader>,
}

impl ChainIterator {
    #[allow(unknown_lints)]
    #[allow(needless_pass_by_value)]
    pub fn new(table_actor: ActorRef<Protocol>, chain_header: &Option<ChainHeader>) -> ChainIterator {
        ChainIterator {
            current: chain_header.clone(),
            table_actor: table_actor.clone(),
        }
    }
}

impl Iterator for ChainIterator {
    type Item = ChainHeader;

    /// May panic if there is an underlying error in the table
    fn next(&mut self) -> Option<ChainHeader> {
        let previous = self.current.take();

        self.current = previous
            .as_ref()
            .and_then(|h| h.link())
            // @TODO should this panic?
            // @see https://github.com/holochain/holochain-rust/issues/146
            .and_then(|header_address| {
                let header_entry = &self.table_actor.entry(&header_address)
                                    .expect("getting from a table shouldn't fail")
                                    .expect("getting from a table shouldn't fail");
                // Recreate the Pair from the HeaderEntry
                let chain_header = ChainHeader::from_entry(header_entry);
                chain_header
            });
        previous
    }
}

#[derive(Clone, Debug)]
pub struct Chain {
    chain_actor: ActorRef<Protocol>,
    table_actor: ActorRef<Protocol>,
}

impl PartialEq for Chain {
    fn eq(&self, other: &Chain) -> bool {
        // list linking by header addresses ensures that if the tops match the whole chain matches
        self.top_pair() == other.top_pair()
    }
}

impl Eq for Chain {}

/// Turns a chain into an iterator over it's Pairs
impl IntoIterator for Chain {
    type Item = ChainHeader;
    type IntoIter = ChainIterator;

    /// returns a ChainIterator that provides cloned Pairs from the underlying HashTable
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Chain {
    pub fn new(table: ActorRef<Protocol>) -> Chain {
        Chain {
            chain_actor: ChainActor::new_ref(),
            table_actor: table.clone(),
        }
    }

    /// Create the next commitable Header for the chain.
    /// a Header is immutable, but the chain is mutable if chain.commit_*() is used.
    /// this means that a header becomes invalid and useless as soon as the chain is mutated
    /// the only valid usage of a header is to immediately commit it onto a chain in a Pair.
    /// normally (outside unit tests) the generation of valid headers is internal to the
    /// chain::SourceChain trait and should not need to be handled manually
    ///
    /// @see chain::pair::Pair
    /// @see chain::entry::Entry
    pub fn create_next_chain_header(&self, entry_header: &EntryHeader) -> ChainHeader {
        ChainHeader::new(
            entry_header.entry_type(),
            // @TODO implement timestamps
            // https://github.com/holochain/holochain-rust/issues/70
            &String::new(),
            self.top_chain_header()
                .expect("could not get top pair when building header")
                .as_ref()
                .map(|chain_header| chain_header.address()),
            &entry_header.entry_address(),
            // @TODO implement signatures
            // https://github.com/holochain/holochain-rust/issues/71
            &String::new(),
            self
                .top_chain_header_of_type(&entry_header.entry_type())
                // @TODO inappropriate expect()?
                // @see https://github.com/holochain/holochain-rust/issues/147
                .map(|chain_header| chain_header.address()),
        )
    }

    /// returns a ChainIterator that provides cloned Pairs from the underlying HashTable
    fn iter(&self) -> ChainIterator {
        ChainIterator::new(
            self.table(),
            &self
                .top_pair()
                .expect("could not get top pair when building iterator"),
        )
    }

    /// restore canonical JSON chain
    /// can't implement json::FromJson due to Chain's need for a table actor
    /// @TODO accept canonical JSON
    /// @see https://github.com/holochain/holochain-rust/issues/75
    pub fn import_from_json(table: ActorRef<Protocol>, s: &str) -> Self {
        // @TODO inappropriate unwrap?
        // @see https://github.com/holochain/holochain-rust/issues/168
        let mut as_seq: Vec<ChainHeader> = serde_json::from_str(s).expect("argument should be valid json");
        as_seq.reverse();

        let mut chain = Chain::new(table);

        for chain_header in as_seq {
            chain.push_chain_header(&chain_header).expect("pair should be valid");
        }
        chain
    }

    /// table getter
    /// returns a reference to the underlying HashTable
    pub fn table(&self) -> ActorRef<Protocol> {
        self.table_actor.clone()
    }
}

// @TODO should SourceChain have a bound on HashTable for consistency?
// @see https://github.com/holochain/holochain-rust/issues/261
pub trait SourceChain {
    /// sets an option for the top Pair
    fn set_top_chain_header(&self, &Option<ChainHeader>) -> Result<Option<ChainHeader>, HolochainError>;
    /// returns an option for the top Pair
    fn top_chain_header(&self) -> Result<Option<ChainHeader>, HolochainError>;
    /// get the top Pair by Entry type
    fn top_chain_header_of_type(&self, entry_type: &EntryType) -> Option<ChainHeader>;

    /// push a new Entry on to the top of the Chain.
    /// The Pair for the new Entry is generated and validated against the current top
    /// Pair to ensure the chain links up correctly across the underlying table data
    /// the newly created and pushed Pair is returned.
    fn push_entry(&mut self, entry_header: &EntryHeader, entry: &Entry) -> Result<ChainHeader, HolochainError>;
}

impl SourceChain for Chain {
    fn set_top_chain_header(&self, chain_header: &Option<ChainHeader>) -> Result<Option<ChainHeader>, HolochainError> {
        self.chain_actor.set_top_chain_header(&chain_header)
    }

    fn top_chain_header(&self) -> Result<Option<ChainHeader>, HolochainError> {
        self.chain_actor.top_chain_header()
    }

    fn top_chain_header_of_type(&self, entry_type: &EntryType) -> Option<ChainHeader> {
        self.iter().find(|chain_header| chain_header.entry_type() == entry_type)
    }

    /// Assumes that the entry is already in the CAS!
    /// the EntryHeader is simply incorporated into a ChainHeader and set as the top
    fn push_entry(&mut self, entry_header: &EntryHeader, entry: &Entry) -> Result<ChainHeader, HolochainError> {
        let chain_header = self.create_next_chain_header(entry_header);
        self.table_actor.put_entry(chain_header)?;

        // @TODO if top pair set fails but commit succeeds?
        // @see https://github.com/holochain/holochain-rust/issues/259
        self.set_top_chain_header(&Some(chain_header.clone()))?;

        Ok(chain_header)
    }

    entry(&self, address: &Address) -> Result<Entry, HolochainError> {

    }
}

impl ToJson for Chain {
    /// get the entire chain, top to bottom as a JSON array or canonical pairs
    /// @TODO return canonical JSON
    /// @see https://github.com/holochain/holochain-rust/issues/75
    fn to_json(&self) -> Result<String, HolochainError> {
        let as_seq = self.iter().collect::<Vec<ChainHeader>>();
        Ok(serde_json::to_string(&as_seq)?)
    }
}

#[cfg(test)]
pub mod tests {

    use super::Chain;
    use chain::{
        pair::{tests::test_pair, Pair},
        SourceChain,
    };
    use hash::HashString;
    use hash_table::{
        actor::tests::test_table_actor,
        entry::tests::{test_entry, test_entry_a, test_entry_b, test_type_a, test_type_b},
        HashTable,
    };
    use json::ToJson;
    use key::Key;
    use std::thread;

    /// builds a dummy chain for testing
    pub fn test_chain() -> Chain {
        Chain::new(test_table_actor())
    }

    #[test]
    /// smoke test for new chains
    fn new() {
        test_chain();
    }

    #[test]
    /// test chain equality
    fn eq() {
        let mut chain1 = test_chain();
        let mut chain2 = test_chain();
        let mut chain3 = test_chain();

        let entry_a = test_entry_a();
        let entry_b = test_entry_b();

        chain1
            .push_entry(&entry_a)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        chain2
            .push_entry(&entry_a)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        chain3
            .push_entry(&entry_b)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(chain1.top_pair(), chain2.top_pair());
        assert_eq!(chain1, chain2);

        assert_ne!(chain1, chain3);
        assert_ne!(chain2, chain3);
    }

    #[test]
    /// tests for chain.top_pair()
    fn top_pair() {
        let mut chain = test_chain();

        assert_eq!(
            None,
            chain
                .top_pair()
                .expect("could not get top pair from test chain")
        );

        let entry_a = test_entry_a();
        let entry_b = test_entry_b();

        let pair_a = chain
            .push_entry(&entry_a)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        assert_eq!(&entry_a, pair_a.entry());
        let top_pair = chain.top_pair().expect("should have commited entry");
        assert_eq!(Some(pair_a), top_pair);

        let pair_b = chain
            .push_entry(&entry_b)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        assert_eq!(&entry_b, pair_b.entry());
        let top_pair = chain.top_pair().expect("should have commited entry");
        assert_eq!(Some(pair_b), top_pair);
    }

    #[test]
    /// tests that the chain state is consistent across clones
    fn clone_safe() {
        let chain_1 = test_chain();
        let mut chain_2 = chain_1.clone();
        let test_pair = test_pair();

        assert_eq!(
            None,
            chain_1
                .top_pair()
                .expect("could not get top pair for chain 1")
        );
        assert_eq!(
            None,
            chain_2
                .top_pair()
                .expect("could not get top pair for chain 2")
        );

        let pair = chain_2.push_pair(&test_pair).unwrap();

        assert_eq!(
            Some(pair.clone()),
            chain_2
                .top_pair()
                .expect("could not get top pair after pushing to chain 2")
        );
        assert_eq!(
            chain_1
                .top_pair()
                .expect("could not get top pair for comparing chain 1"),
            chain_2
                .top_pair()
                .expect("could not get top pair when comparing chain 2")
        );
    }

    #[test]
    // test that adding something to the chain adds to the table
    fn table_put() {
        let table_actor = test_table_actor();
        let mut chain = Chain::new(table_actor.clone());

        let pair = chain
            .push_pair(&test_pair())
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        let table_entry = table_actor
            .entry(&pair.entry().key())
            .expect("getting an entry from a table in a chain shouldn't fail")
            .expect("table should have entry");
        let chain_entry = chain
            .entry(&pair.entry().key())
            .expect("getting an entry from a chain shouldn't fail");

        assert_eq!(pair.entry(), &table_entry);
        assert_eq!(table_entry, chain_entry);
    }

    #[test]
    fn can_commit_entry() {
        let mut chain = test_chain();

        assert_eq!(
            None,
            chain
                .top_pair()
                .expect("could not get top pair for test chain")
        );

        // chain top, pair entry and headers should all line up after a push
        let entry_1 = test_entry_a();
        let pair_1 = chain
            .push_entry(&entry_1)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");

        assert_eq!(
            Some(&pair_1),
            chain
                .top_pair()
                .expect("could not get top pair for pair 1")
                .as_ref()
        );
        assert_eq!(&entry_1, pair_1.entry());
        assert_eq!(entry_1.key(), pair_1.entry().key());

        // we should be able to do it again
        let entry_2 = test_entry_b();
        let pair_2 = chain
            .push_entry(&entry_2)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");

        assert_eq!(
            Some(&pair_2),
            chain
                .top_pair()
                .expect("could not get top pair for pair 2")
                .as_ref()
        );
        assert_eq!(&entry_2, pair_2.entry());
        assert_eq!(entry_2.key(), pair_2.entry().key());
    }

    #[test]
    fn validate() {
        println!("can_validate: Empty Chain");
        let mut chain = test_chain();
        assert!(chain.validate());

        println!("can_validate: Chain One");
        let e1 = test_entry_a();
        chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");
        assert!(chain.validate());

        println!("can_validate: Chain with Two");
        let e2 = test_entry_b();
        chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");
        assert!(chain.validate());
    }

    #[test]
    /// test chain.push() and chain.get() together
    fn round_trip() {
        let mut chain = test_chain();
        let entry = test_entry();
        let pair = chain
            .push_entry(&entry)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");

        assert_eq!(
            entry,
            chain
                .entry(&pair.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
    }

    #[test]
    /// show that we can push the chain a bit without issues e.g. async
    fn round_trip_stress_test() {
        let h = thread::spawn(|| {
            let mut chain = test_chain();
            let entry = test_entry();

            for _ in 1..100 {
                let pair = chain.push_entry(&entry).unwrap();
                assert_eq!(Some(pair.entry().clone()), chain.entry(&pair.entry().key()),);
            }
        });
        h.join().unwrap();
    }

    #[test]
    /// test chain.iter()
    fn iter() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        let p1 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let p2 = chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(vec![p2, p1], chain.iter().collect::<Vec<Pair>>());
    }

    #[test]
    /// test chain.iter() functional interface
    fn iter_functional() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        let p1 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let _p2 = chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let p3 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(
            vec![p3, p1],
            chain
                .iter()
                .filter(|p| p.entry().entry_type() == "testEntryType")
                .collect::<Vec<Pair>>()
        );
    }

    #[test]
    fn entry_advance() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        let p1 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let p2 = chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(
            p1.entry().clone(),
            chain
                .entry(&p1.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );

        let p3 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(None, chain.entry(&HashString::new()));
        assert_eq!(
            p3.entry().clone(),
            chain
                .entry(&p1.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p2.entry().clone(),
            chain
                .entry(&p2.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p3.entry().clone(),
            chain
                .entry(&p3.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );

        assert_eq!(
            p1,
            chain
                .pair(&p1.key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p2,
            chain
                .pair(&p2.key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p3,
            chain
                .pair(&p3.key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
    }

    #[test]
    fn entry() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        let p1 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");
        let p2 = chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");
        let p3 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exclusively owned chain shouldn't fail");

        assert_eq!(None, chain.entry(&HashString::new()));
        // @TODO at this point we have p3 with the same entry key as p1...
        assert_eq!(
            p3.entry().clone(),
            chain
                .entry(&p1.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p2.entry().clone(),
            chain
                .entry(&p2.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
        assert_eq!(
            p3.entry().clone(),
            chain
                .entry(&p3.entry().key())
                .expect("getting an entry from a chain shouldn't fail"),
        );
    }

    #[test]
    fn top_pair_of_type() {
        let mut chain = test_chain();

        assert_eq!(None, chain.top_pair_of_type(&test_type_a()));
        assert_eq!(None, chain.top_pair_of_type(&test_type_b()));

        let entry1 = test_entry_a();
        let entry2 = test_entry_b();

        // type a should be p1
        // type b should be None
        let pair1 = chain
            .push_entry(&entry1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        assert_eq!(
            Some(&pair1),
            chain.top_pair_of_type(&test_type_a()).as_ref()
        );
        assert_eq!(None, chain.top_pair_of_type(&test_type_b()));

        // type a should still be pair1
        // type b should be p2
        let pair2 = chain
            .push_entry(&entry2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        assert_eq!(
            Some(&pair1),
            chain.top_pair_of_type(&test_type_a()).as_ref()
        );
        assert_eq!(
            Some(&pair2),
            chain.top_pair_of_type(&test_type_b()).as_ref()
        );

        // type a should be pair3
        // type b should still be pair2
        let pair3 = chain
            .push_entry(&entry1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        assert_eq!(
            Some(&pair3),
            chain.top_pair_of_type(&test_type_a()).as_ref()
        );
        assert_eq!(
            Some(&pair2),
            chain.top_pair_of_type(&test_type_b()).as_ref()
        );
    }

    #[test]
    /// test IntoIterator implementation
    fn into_iter() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        let p1 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let p2 = chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        let p3 = chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        // into_iter() returns clones of pairs
        assert_eq!(vec![p3, p2, p1], chain.into_iter().collect::<Vec<Pair>>());
    }

    #[test]
    /// test to_json() and from_json() implementation
    fn json_round_trip() {
        let mut chain = test_chain();

        let e1 = test_entry_a();
        let e2 = test_entry_b();

        chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        chain
            .push_entry(&e2)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");
        chain
            .push_entry(&e1)
            .expect("pushing a valid entry to an exlusively owned chain shouldn't fail");

        let expected_json = "[{\"header\":{\"entry_type\":\"testEntryType\",\"timestamp\":\"\",\"link\":\"QmdEVL9whBj1Tr9VoR6BzmVjrgyPdN5vJ2bbdQdwwfQ9Uq\",\"entry_hash\":\"QmbXSE38SN3SuJDmHKSSw5qWWegvU7oTxrLDRavWjyxMrT\",\"entry_signature\":\"\",\"link_same_type\":\"QmawqBCVVap9KdaakqEHF4JzUjjLhmR7DpM5jgJko8j1rA\"},\"entry\":{\"content\":\"test entry content\",\"entry_type\":\"testEntryType\"}},{\"header\":{\"entry_type\":\"testEntryTypeB\",\"timestamp\":\"\",\"link\":\"QmU8vuUfCQGBb8SUdWjKqmSmsWwXBn4AJPb3HLb8cqWtYn\",\"entry_hash\":\"QmPz5jKXsxq7gPVAbPwx5gD2TqHfqB8n25feX5YH18JXrT\",\"entry_signature\":\"\",\"link_same_type\":null},\"entry\":{\"content\":\"other test entry content\",\"entry_type\":\"testEntryTypeB\"}},{\"header\":{\"entry_type\":\"testEntryType\",\"timestamp\":\"\",\"link\":null,\"entry_hash\":\"QmbXSE38SN3SuJDmHKSSw5qWWegvU7oTxrLDRavWjyxMrT\",\"entry_signature\":\"\",\"link_same_type\":null},\"entry\":{\"content\":\"test entry content\",\"entry_type\":\"testEntryType\"}}]"
        ;
        assert_eq!(
            expected_json,
            chain.to_json().expect("chain shouldn't fail to serialize")
        );

        let table_actor = test_table_actor();
        assert_eq!(chain, Chain::from_json(table_actor, expected_json));
    }

}
