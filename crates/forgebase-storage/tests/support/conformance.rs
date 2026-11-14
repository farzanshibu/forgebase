//! The driver conformance suite: one set of expectations every
//! [`StorageDriver`] must meet, run against the local driver and against the
//! S3, GCS and Azure drivers on their mock servers.

use forgebase_storage::driver::collect;
use forgebase_storage::{StorageDriver, StorageError, body_of};

use super::{chunks_of, failing_after, pattern};

async fn read(d: &dyn StorageDriver, key: &str, range: Option<(u64, u64)>) -> Vec<u8> {
    collect(d.get(key, range).await.unwrap()).await.unwrap()
}

/// Runs the whole suite. `big` is the size of a body that does not fit one
/// upload chunk of the driver (so it takes the multipart / resumable / block
/// path); `driver_name` is what `name()` must answer.
pub async fn run(d: &dyn StorageDriver, driver_name: &str, big: usize) {
    assert_eq!(d.name(), driver_name);
    d.ping().await.expect("ping");

    // Missing blobs.
    assert_eq!(
        d.head("blobs/aa/missing").await.unwrap(),
        None,
        "head of a missing blob"
    );
    assert!(
        matches!(d.get("blobs/aa/missing", None).await, Err(StorageError::NotFound(_))),
        "get of a missing blob"
    );
    d.delete("blobs/aa/missing")
        .await
        .expect("deleting a missing blob succeeds");

    // Small blob: put, head, whole read, ranges (clamped at the end).
    let small = pattern(1000);
    assert_eq!(d.put("blobs/aa/small", body_of(small.clone())).await.unwrap(), 1000);
    let h = d.head("blobs/aa/small").await.unwrap().expect("head");
    assert_eq!((h.key.as_str(), h.size), ("blobs/aa/small", 1000));
    assert_eq!(read(d, "blobs/aa/small", None).await, small);
    assert_eq!(read(d, "blobs/aa/small", Some((10, 50))).await, small[10..50]);
    assert_eq!(read(d, "blobs/aa/small", Some((990, 5000))).await, small[990..]);
    assert_eq!(read(d, "blobs/aa/small", Some((5, 5))).await, Vec::<u8>::new());

    // Empty blob.
    assert_eq!(d.put("blobs/aa/empty", body_of(Vec::new())).await.unwrap(), 0);
    assert_eq!(d.head("blobs/aa/empty").await.unwrap().unwrap().size, 0);
    assert!(read(d, "blobs/aa/empty", None).await.is_empty());

    // A big blob in odd-sized chunks (multipart / resumable / blocks).
    let data = pattern(big);
    assert_eq!(
        d.put("blobs/bb/big", chunks_of(data.clone(), 7001)).await.unwrap(),
        big as u64
    );
    assert_eq!(d.head("blobs/bb/big").await.unwrap().unwrap().size, big as u64);
    assert!(read(d, "blobs/bb/big", None).await == data, "the whole big blob");
    let (a, b) = (big / 3, big / 3 * 2 + 17);
    assert_eq!(
        read(d, "blobs/bb/big", Some((a as u64, b as u64))).await,
        data[a..b],
        "a range across chunks"
    );

    // Overwriting replaces.
    let other = pattern(333);
    d.put("blobs/bb/big", body_of(other.clone())).await.unwrap();
    assert_eq!(read(d, "blobs/bb/big", None).await, other);
    assert_eq!(d.head("blobs/bb/big").await.unwrap().unwrap().size, 333);

    // A stream that fails midway leaves nothing under the key, whichever
    // upload path it took.
    for (key, len) in [("blobs/cc/halfway", big + big / 2), ("blobs/cc/early", 10)] {
        let r = d.put(key, failing_after(pattern(len), 5003)).await;
        assert!(r.is_err(), "a failing stream fails the put ({key})");
        assert_eq!(d.head(key).await.unwrap(), None, "nothing left under {key}");
    }

    // Copy and rename.
    d.copy("blobs/aa/small", "blobs/dd/copy").await.expect("copy");
    assert_eq!(read(d, "blobs/dd/copy", None).await, small);
    assert_eq!(read(d, "blobs/aa/small", None).await, small, "the source stays");
    assert!(
        matches!(
            d.copy("blobs/aa/missing", "blobs/dd/nope").await,
            Err(StorageError::NotFound(_))
        ),
        "copying a missing blob"
    );
    assert_eq!(d.head("blobs/dd/nope").await.unwrap(), None);
    d.rename("blobs/dd/copy", "blobs/dd/moved").await.expect("rename");
    assert_eq!(d.head("blobs/dd/copy").await.unwrap(), None);
    assert_eq!(read(d, "blobs/dd/moved", None).await, small);

    // Keys with characters that need encoding.
    let odd = "staging/a b+c&d=e/ü?#%.bin";
    d.put(odd, body_of(small.clone())).await.unwrap();
    assert_eq!(read(d, odd, None).await, small);
    assert_eq!(d.head(odd).await.unwrap().unwrap().key, odd);
    d.delete(odd).await.unwrap();
    assert_eq!(d.head(odd).await.unwrap(), None);

    // Listing: prefix filter, key order, paging.
    for i in 0..7 {
        d.put(&format!("many/k{i:02}"), body_of(vec![i as u8; 10 + i]))
            .await
            .unwrap();
    }
    let many = d.list("many/").await.unwrap();
    let keys: Vec<&str> = many.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "many/k00", "many/k01", "many/k02", "many/k03", "many/k04", "many/k05", "many/k06"
        ]
    );
    assert_eq!(many[3].size, 13);
    let blobs = d.list("blobs/").await.unwrap();
    let keys: Vec<&str> = blobs.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        ["blobs/aa/empty", "blobs/aa/small", "blobs/bb/big", "blobs/dd/moved"],
        "prefix filter, sorted, no leftovers of failed uploads"
    );
    assert!(d.list("nothing/").await.unwrap().is_empty());

    // Deleting.
    d.delete("blobs/aa/small").await.unwrap();
    assert_eq!(d.head("blobs/aa/small").await.unwrap(), None);
    d.delete("blobs/aa/small").await.expect("deleting twice");
}
