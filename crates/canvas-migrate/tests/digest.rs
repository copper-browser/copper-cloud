//! Properties of the §6.3 canonical digest (`common::digest`): independent of update order,
//! snapshot compaction and root typing; sensitive to content.

mod common;

use common::digest::{canonical_json, digest};
use yrs::types::text::Text as _;
use yrs::updates::decoder::Decode as _;
use yrs::{
    any, Any, Array as _, Doc, Map as _, MapPrelim, Out, ReadTxn as _, StateVector, Transact as _,
    Update,
};

fn doc_from(updates: &[&[u8]]) -> Doc {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        for u in updates {
            txn.apply_update(Update::decode_v1(u).unwrap()).unwrap();
        }
    }
    doc
}

fn edit(doc: &Doc, f: impl FnOnce(&Doc)) -> Vec<u8> {
    let before = doc.transact().state_vector();
    f(doc);
    doc.transact().encode_state_as_update_v1(&before)
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for p in permutations(n - 1) {
        for i in 0..=p.len() {
            let mut q = p.clone();
            q.insert(i, n - 1);
            out.push(q);
        }
    }
    out
}

fn compacted(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

/// Updates from two concurrent clients over every root kind.
fn history() -> Vec<Vec<u8>> {
    let a = Doc::with_client_id(1);
    let b = Doc::with_client_id(2);
    let mut out = Vec::new();
    out.push(edit(&a, |d| {
        let meta = d.get_or_insert_map("meta");
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        meta.insert(&mut t, "name", "Board");
        shapes.insert(
            &mut t,
            "s1",
            MapPrelim::from([("x", Any::from(1)), ("y", Any::from(2.5))]),
        );
        shapes.insert(&mut t, "gone", any!({"type": "sticky"}));
    }));
    out.push(edit(&b, |d| {
        let chat = d.get_or_insert_array("chat");
        let notes = d.get_or_insert_text("notes");
        let mut t = d.transact_mut();
        chat.push_back(&mut t, any!({"id": "m1", "text": "hi \u{2028} </script>"}));
        notes.insert(&mut t, 0, "naïve ✓");
    }));
    {
        let mut t = b.transact_mut();
        t.apply_update(Update::decode_v1(&out[0]).unwrap()).unwrap();
    }
    out.push(edit(&b, |d| {
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        if let Some(Out::YMap(s1)) = shapes.get(&t, "s1") {
            s1.insert(&mut t, "x", 99);
        }
        shapes.remove(&mut t, "gone");
    }));
    out.push(edit(&a, |d| {
        let meta = d.get_or_insert_map("meta");
        let mut t = d.transact_mut();
        meta.insert(&mut t, "thumb", Any::Buffer(vec![1_u8, 2, 3].into()));
        meta.insert(&mut t, "big", Any::Number(yrs::Number::Int(7)));
    }));
    out
}

#[test]
fn order_compaction_and_typing_do_not_matter() {
    let h = history();
    let refs: Vec<&[u8]> = h.iter().map(Vec::as_slice).collect();
    let forward = doc_from(&refs);
    let expected = digest(&forward);

    // Every permutation of the four updates. Causal orders (u1,u2 ≺ u3; u1 ≺ u4) always
    // integrate completely. yrs 0.28 can leave structs pending for some non-causal orders
    // (e.g. exact reverse: `has_missing_updates()` stays true) — then the document really is
    // different until the missing pieces are re-delivered. The property is therefore: every
    // *complete* application yields the same digest.
    let mut complete = 0;
    for order in permutations(4) {
        let us: Vec<&[u8]> = order.iter().map(|i| refs[*i]).collect();
        let doc = doc_from(&us);
        let pos = |u: usize| order.iter().position(|x| *x == u).unwrap();
        let causal = pos(0) < pos(2) && pos(1) < pos(2) && pos(0) < pos(3);
        if doc.transact().has_missing_updates() {
            assert!(!causal, "a causal order {order:?} left updates pending");
            // Re-delivering everything completes it.
            let again = doc_from(&[us.clone(), refs.clone()].concat());
            assert_eq!(digest(&again), expected, "{order:?} re-delivered");
        } else {
            complete += 1;
            assert_eq!(digest(&doc), expected, "order {order:?}");
        }
    }
    assert!(complete >= 6, "{complete}");

    // One compacted snapshot, and a snapshot of a prefix followed by the tail.
    assert_eq!(
        digest(&doc_from(&[&compacted(&forward)])),
        expected,
        "compacted"
    );
    let prefix = compacted(&doc_from(&refs[..2]));
    assert_eq!(
        digest(&doc_from(&[&prefix, refs[2], refs[3]])),
        expected,
        "snapshot + tail"
    );

    // A reader that typed its roots (like a server that called get_or_insert_*).
    let typed = Doc::new();
    typed.get_or_insert_map("meta");
    typed.get_or_insert_map("shapes");
    typed.get_or_insert_array("chat");
    typed.get_or_insert_text("notes");
    typed.get_or_insert_map("never-used");
    {
        let mut t = typed.transact_mut();
        for u in &refs {
            t.apply_update(Update::decode_v1(u).unwrap()).unwrap();
        }
    }
    assert_eq!(digest(&typed), expected, "typed roots, plus an empty one");
}

#[test]
fn content_changes_change_the_digest() {
    let h = history();
    let refs: Vec<&[u8]> = h.iter().map(Vec::as_slice).collect();
    let doc = doc_from(&refs);
    let before = digest(&doc);
    let more = edit(&doc, |d| {
        let meta = d.get_or_insert_map("meta");
        let mut t = d.transact_mut();
        meta.insert(&mut t, "name", "Board 2");
    });
    assert_ne!(digest(&doc), before);
    assert_eq!(
        digest(&doc_from(&[&compacted(&doc_from(&refs)), &more])),
        digest(&doc)
    );
    // Missing an update is detected.
    assert_ne!(digest(&doc_from(&refs[..3])), before);
}

#[test]
fn canonical_form_is_exact() {
    let h = history();
    let refs: Vec<&[u8]> = h.iter().map(Vec::as_slice).collect();
    let doc = doc_from(&refs);
    let json = canonical_json(&doc.transact());
    assert_eq!(
        json,
        concat!(
            r#"{"chat":[{"id":"m1","text":"hi "#,
            "\u{2028}",
            r#" </script>"}],"#,
            r#""meta":{"big":7.0,"name":"Board","thumb":"AQID"},"#,
            r#""notes":"naïve ✓","#,
            r#""shapes":{"s1":{"x":99.0,"y":2.5}}}"#
        )
    );
    // Valid JSON, and the digest is SHA-256 of exactly these bytes.
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["shapes"]["s1"]["x"], 99.0);
    assert_eq!(digest(&doc).len(), 64);
}

#[test]
fn empty_documents_and_empty_roots() {
    let empty = Doc::new();
    assert_eq!(canonical_json(&empty.transact()), "{}");
    // e3b0… is SHA-256("") — the digest is over "{}", not the empty string.
    assert_eq!(
        digest(&empty),
        "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
    );
    // A root whose only entry was deleted renders like no root at all.
    let doc = Doc::new();
    let m = doc.get_or_insert_map("shapes");
    {
        let mut t = doc.transact_mut();
        m.insert(&mut t, "a", 1);
    }
    {
        let mut t = doc.transact_mut();
        m.remove(&mut t, "a");
    }
    assert_eq!(digest(&doc), digest(&empty));
    assert_eq!(digest(&doc_from(&[&compacted(&doc)])), digest(&empty));
}
