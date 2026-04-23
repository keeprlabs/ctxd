use criterion::{criterion_group, criterion_main, Criterion};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store::EventStore;

fn make_event(subject: &str, index: usize) -> Event {
    Event::new(
        "ctxd://bench".to_string(),
        Subject::new(subject).unwrap(),
        "bench".to_string(),
        serde_json::json!({"index": index, "content": "benchmark event data payload"}),
    )
}

fn bench_append(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("append_single", |b| {
        b.iter(|| {
            rt.block_on(async {
                let store = EventStore::open_memory().await.unwrap();
                let event = make_event("/bench/single", 0);
                store.append(event).await.unwrap();
            });
        });
    });
}

fn bench_append_100(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("append_100_sequential", |b| {
        b.iter(|| {
            rt.block_on(async {
                let store = EventStore::open_memory().await.unwrap();
                for i in 0..100 {
                    let event = make_event("/bench/sequential", i);
                    store.append(event).await.unwrap();
                }
            });
        });
    });
}

fn bench_read_exact(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        let event = make_event("/bench/read-exact", 0);
        store.append(event).await.unwrap();
        store
    });

    let subject = Subject::new("/bench/read-exact").unwrap();
    c.bench_function("read_exact_1_event", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.read(&subject, false).await.unwrap();
            });
        });
    });
}

fn bench_read_recursive_100(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..100 {
            let event = make_event(&format!("/bench/rec100/item{i}"), i);
            store.append(event).await.unwrap();
        }
        store
    });

    let subject = Subject::new("/bench/rec100").unwrap();
    c.bench_function("read_recursive_100_events", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.read(&subject, true).await.unwrap();
            });
        });
    });
}

fn bench_read_recursive_1000(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..1000 {
            let event = make_event(&format!("/bench/rec1000/item{i}"), i);
            store.append(event).await.unwrap();
        }
        store
    });

    let subject = Subject::new("/bench/rec1000").unwrap();
    c.bench_function("read_recursive_1000_events", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.read(&subject, true).await.unwrap();
            });
        });
    });
}

fn bench_search_100(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..100 {
            let event = Event::new(
                "ctxd://bench".to_string(),
                Subject::new(&format!("/bench/search100/item{i}")).unwrap(),
                "document".to_string(),
                serde_json::json!({"content": format!("searchable document number {i} with benchmark data")}),
            );
            store.append(event).await.unwrap();
        }
        store
    });

    c.bench_function("search_fts_over_100_events", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.search("searchable benchmark", None).await.unwrap();
            });
        });
    });
}

fn bench_search_10000(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..10_000 {
            let event = Event::new(
                "ctxd://bench".to_string(),
                Subject::new(&format!("/bench/search10k/item{i}")).unwrap(),
                "document".to_string(),
                serde_json::json!({"content": format!("searchable document number {i} with benchmark data")}),
            );
            store.append(event).await.unwrap();
        }
        store
    });

    c.bench_function("search_fts_over_10000_events", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.search("searchable benchmark", None).await.unwrap();
            });
        });
    });
}

fn bench_kv_get(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..100 {
            let event = make_event("/bench/kv-get", i);
            store.append(event).await.unwrap();
        }
        store
    });

    c.bench_function("kv_get_latest_value", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.kv_get("/bench/kv-get").await.unwrap();
            });
        });
    });
}

criterion_group!(
    benches,
    bench_append,
    bench_append_100,
    bench_read_exact,
    bench_read_recursive_100,
    bench_read_recursive_1000,
    bench_search_100,
    bench_search_10000,
    bench_kv_get,
);
criterion_main!(benches);
