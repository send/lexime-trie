use criterion::{black_box, criterion_group, criterion_main, Criterion};
use lemma::DoubleArray;

// ── Hand-rolled LCG (no external deps) ──────────────────────────────────────

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
    /// Returns a value in [0, bound).
    fn next_range(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

// ── Hiragana char keys (50K) ────────────────────────────────────────────────

/// 'あ' (U+3041) .. 'ん' (U+3093) — 83 hiragana codepoints
const HIRAGANA_START: u32 = 0x3041;
const HIRAGANA_COUNT: u64 = 83; // U+3041..=U+3093

fn generate_char_keys(n: usize, seed: u64) -> Vec<Vec<char>> {
    let mut rng = Lcg::new(seed);
    let mut set = std::collections::BTreeSet::new();
    while set.len() < n {
        let len = (rng.next_range(7) + 2) as usize; // 2..=8
        let key: Vec<char> = (0..len)
            .map(|_| {
                let cp = HIRAGANA_START + rng.next_range(HIRAGANA_COUNT) as u32;
                char::from_u32(cp).unwrap()
            })
            .collect();
        set.insert(key);
    }
    set.into_iter().collect() // already sorted & unique
}

// ── Romaji u8 keys ──────────────────────────────────────────────────────────

fn romaji_keys() -> Vec<&'static [u8]> {
    vec![
        b"a", b"ba", b"be", b"bi", b"bo", b"bu", b"chi", b"da", b"de", b"di", b"do", b"du", b"fu",
        b"ga", b"ge", b"gi", b"go", b"gu", b"ha", b"he", b"hi", b"ho", b"hu", b"i", b"ja", b"ji",
        b"jo", b"ju", b"ka", b"ke", b"ki", b"ko", b"ku", b"ma", b"me", b"mi", b"mo", b"mu", b"n",
        b"na", b"ne", b"ni", b"no", b"nu", b"o", b"pa", b"pe", b"pi", b"po", b"pu", b"ra", b"re",
        b"ri", b"ro", b"ru", b"sa", b"se", b"sha", b"shi", b"sho", b"shu", b"si", b"so", b"su",
        b"ta", b"te", b"ti", b"to", b"tsu", b"tu", b"u", b"wa", b"wo", b"ya", b"yo", b"yu", b"za",
        b"ze", b"zi", b"zo", b"zu",
    ]
}

// ── Benchmarks ──────────────────────────────────────────────────────────────

fn bench_build(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    c.bench_function("build_50k_char", |b| {
        b.iter(|| DoubleArray::<char>::build(black_box(&keys)));
    });

    let romaji = romaji_keys();
    c.bench_function("build_romaji_u8", |b| {
        b.iter(|| DoubleArray::<u8>::build(black_box(&romaji)));
    });
}

fn bench_serial(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    let da = DoubleArray::<char>::build(&keys);
    let bytes = da.as_bytes();
    c.bench_function("serial_round_trip", |b| {
        b.iter(|| {
            let encoded = black_box(&da).as_bytes();
            let _ = DoubleArray::<char>::from_bytes(black_box(&encoded)).unwrap();
        });
    });
    c.bench_function("serial_from_bytes", |b| {
        b.iter(|| {
            let _ = DoubleArray::<char>::from_bytes(black_box(&bytes)).unwrap();
        });
    });
}

fn bench_exact_match(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    let da = DoubleArray::<char>::build(&keys);

    // Pick 1000 hit keys and 1000 miss keys
    let mut rng = Lcg::new(123);
    let hit_keys: Vec<&Vec<char>> = (0..1000)
        .map(|_| &keys[rng.next_range(keys.len() as u64) as usize])
        .collect();
    let miss_keys: Vec<Vec<char>> = (0..1000)
        .map(|_| {
            // Generate a random key unlikely to be in the trie
            let len = (rng.next_range(7) + 2) as usize;
            (0..len)
                .map(|_| {
                    let cp = 0x30A0 + rng.next_range(83) as u32; // katakana range — guaranteed miss
                    char::from_u32(cp).unwrap()
                })
                .collect()
        })
        .collect();

    c.bench_function("exact_match_hit_1k", |b| {
        b.iter(|| {
            for key in &hit_keys {
                black_box(da.exact_match(black_box(key)));
            }
        });
    });

    c.bench_function("exact_match_miss_1k", |b| {
        b.iter(|| {
            for key in &miss_keys {
                black_box(da.exact_match(black_box(key)));
            }
        });
    });
}

fn bench_common_prefix_search(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    let da = DoubleArray::<char>::build(&keys);

    // Generate a random 200-char hiragana sentence
    let mut rng = Lcg::new(999);
    let sentence: Vec<char> = (0..200)
        .map(|_| {
            let cp = HIRAGANA_START + rng.next_range(HIRAGANA_COUNT) as u32;
            char::from_u32(cp).unwrap()
        })
        .collect();

    c.bench_function("common_prefix_search_viterbi", |b| {
        b.iter(|| {
            for offset in 0..sentence.len() {
                let results: Vec<_> = da
                    .common_prefix_search(black_box(&sentence[offset..]))
                    .collect();
                black_box(&results);
            }
        });
    });
}

fn bench_predictive_search(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    let da = DoubleArray::<char>::build(&keys);

    // Generate 100 short prefixes (2 chars)
    let mut rng = Lcg::new(777);
    let prefixes: Vec<Vec<char>> = (0..100)
        .map(|_| {
            (0..2)
                .map(|_| {
                    let cp = HIRAGANA_START + rng.next_range(HIRAGANA_COUNT) as u32;
                    char::from_u32(cp).unwrap()
                })
                .collect()
        })
        .collect();

    c.bench_function("predictive_search_2char_prefix", |b| {
        b.iter(|| {
            for prefix in &prefixes {
                let results: Vec<_> = da.predictive_search(black_box(prefix)).collect();
                black_box(&results);
            }
        });
    });
}

fn bench_probe(c: &mut Criterion) {
    let keys = generate_char_keys(50_000, 42);
    let da = DoubleArray::<char>::build(&keys);

    let mut rng = Lcg::new(456);
    let probe_keys: Vec<&Vec<char>> = (0..1000)
        .map(|_| &keys[rng.next_range(keys.len() as u64) as usize])
        .collect();

    c.bench_function("probe_1k", |b| {
        b.iter(|| {
            for key in &probe_keys {
                black_box(da.probe(black_box(key)));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_build,
    bench_serial,
    bench_exact_match,
    bench_common_prefix_search,
    bench_predictive_search,
    bench_probe,
);
criterion_main!(benches);
