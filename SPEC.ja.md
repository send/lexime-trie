# lexime-trie 設計書

> English version: [SPEC.md](SPEC.md)

## 概要

lexime-trie は [lexime](https://github.com/send/lexime) 向けの汎用 Double-Array Trie ライブラリ。
`trie-rs` + `bincode` を置き換え、辞書とローマ字の両方の Trie を統一的に扱う。

## 動機

現在の `TrieDictionary` は `trie-rs::map::Trie<u8, Vec<DictEntry>>` を bincode でシリアライズしている。

| 項目 | 現状 | lexime-trie 導入後 |
|------|------|-------------|
| 辞書ファイルサイズ | ~49MB (bincode) | 実測で確認 |
| ロード時間 | 数百ms (bincode deserialize) | ~5ms (memcpy) |
| ノード表現 | trie-rs 内部構造 (不透明) | `#[repr(C)]` 8B/node |
| 値の格納 | Trie 内部に `Vec<DictEntry>` を保持 | 外部配列 (value_id で参照) |
| ラベル方式 | byte-wise (UTF-8 バイト単位) | **char-wise** (文字単位) |
| 依存クレート | trie-rs, serde, bincode | なし (zero deps) |

char-wise により日本語の common_prefix_search が byte-wise 比で
**1.5-2x 高速** (crawdad ベンチマーク実証済み)。

ローマ字 Trie (`RomajiTrie`) も現在は `HashMap<u8, Node>` ベースだが、
lexime-trie の `DoubleArray<u8>` で置き換えることで統一できる。

## 先行実装

| クレート | ラベル | ノードサイズ | predictive_search | 備考 |
|---------|--------|------------|-------------------|------|
| yada | byte-wise | 8B | なし | darts-clone Rust 移植 |
| crawdad | char-wise | 8B | なし | vibrato (MeCab 2x 速) で採用 |
| trie-rs | byte-wise | LOUDS | あり | 現在 lexime が使用 |
| **lexime-trie** | **char-wise** | **8B + CSR 子配列** | **あり** | crawdad の手法 + predictive_search |

crawdad ベンチマーク (ipadic-neologd, 5.5M keys):

| 操作 | crawdad (char-wise) | yada (byte-wise) | 差 |
|------|-------------------|-----------------|-----|
| exact_match | 9-28 ns | 22-97 ns | 2-3x 速い |
| common_prefix_search | 2.0-2.6 us/line | 3.7-5.3 us/line | 1.5-2x 速い |
| ビルド時間 | 1.93 sec | 34.74 sec | 18x 速い |
| メモリ | 121 MiB | 153 MiB | 20% 小さい |

lexime-trie は crawdad の char-wise + CodeMapper アプローチを採用しつつ、
crawdad にない **predictive_search** (CSR 子配列ベース) と **probe** を追加する。

## データ構造

### Node

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// BASE — XOR ベースの子ノードオフセット (31 bit) | IS_LEAF (1 bit)
    base: u32,
    /// CHECK — 親ノードのインデックス (31 bit) | HAS_LEAF (1 bit)
    check: u32,
}
```

- **8 bytes/node**。キャッシュライン (64B) に 8 ノード収まる
- ノード `n` のラベル `c` の子: `index = base(n) XOR code_map(c)`、`check(index) == n` で検証
- IS_LEAF: base の最上位ビット。立っているとき base の残り 31 bit が value_id
- HAS_LEAF: check の最上位ビット。立っているときターミナル子 (code 0) が存在する
- 子ノード探索は O(1): `base XOR label` で直接インデックス計算

### 子配列 (CSR レイアウト)

```rust
child_offsets: Vec<u32>    // 長さ N + 1
children_list: Vec<u32>    // 長さ E (総エッジ数)
```

親ノード `p` に対し、その子は以下のスライスに並ぶ:

```
children_list[child_offsets[p] .. child_offsets[p+1]]
```

各親のスライス内では code 昇順に並び、ターミナル子 (code 0、存在する場合) は
常に先頭。

- **`Node` 構造体には含めない** — SoA レイアウトで hot path を軽く保つ
- `exact_match` / `common_prefix_search` は `nodes` のみを参照し **8B/node** hot path を保持
- `probe` は `nodes` + `child_offsets` (クエリごとに u32 を 1 ペア読むだけ)
- `predictive_search` は 3 配列すべてを参照
- `nodes[0]` は無効な sentinel、root は `nodes[1]`。
  これにより「root の子」と「未使用スロット」を `check == 0` 一つで曖昧に
  表していた従来設計の問題を構造的に解消

| 操作 | アクセスする配列 |
|------|-----------------|
| `exact_match` | `nodes` のみ |
| `common_prefix_search` | `nodes` のみ |
| `probe` | `nodes` + `child_offsets` |
| `predictive_search` | `nodes` + `child_offsets` + `children_list` |

### CodeMapper (頻度順ラベル再マッピング)

char-wise Double-Array では Unicode 文字をそのままラベルに使うと配列が疎になる。
**CodeMapper** で文字を頻度順の連番にリマップし、密な配列を維持する。

```rust
pub struct CodeMapper {
    /// label (as u32) → remapped code (0 = 未登録)
    table: Vec<u32>,
    /// code → label (as u32)。index 0 は未使用 (ターミナルシンボル)
    reverse_table: Vec<u32>,
    /// ターミナルシンボルを含む総コード数
    alphabet_size: u32,
}
```

- ビルド時に全キーの文字頻度を集計 → 高頻度文字ほど小さい code を割り当て
- 例: ひらがな ~80 種 + カタカナ ~80 種 + 漢字 ~3000 種 → 実効 alphabet size ≈ 4000
- code 0 はターミナルシンボル用に予約
- crawdad の Mapped scheme (Kanda et al. 2023) と同一手法
- `reverse_table` は `predictive_search` でのキー復元に使用
- `DoubleArray<u8>` (ローマ字 Trie) でも頻度順 CodeMapper を使用。
  identity 変換より配列が密になるため有利

### 値の格納 (ターミナルシンボル方式)

Trie は値そのものを持たない。**ターミナルシンボル (code = 0)** を使って値を格納する。

キー "きょう" を value_id=42 で登録するとき、
内部的には `[code('き'), code('ょ'), code('う'), 0]` を挿入する。
ターミナルノードの BASE フィールドに value_id を格納する。

```
通常ノード:
  base  = XOR オフセット (31 bit) | IS_LEAF=0
  check = 親ノードインデックス (31 bit)

ターミナルノード (IS_LEAF = 1):
  value_id = base & 0x7FFF_FFFF  — 31 bit, 最大 ~2G 個
  check    = 親ノードインデックス
```

この方式により **値を持ちつつ子も持つノード** (ExactAndPrefix) を自然に表現できる。
例えばローマ字 Trie で "n" → "ん" かつ "na" → "な" の場合:

```
root --'n'--> N --[0]--> [value_id for "ん"]   (Exact)
                  --'a'--> A --[0]--> [value_id for "な"]
```

ノード N は子 (terminal, 'a') を持つので BASE は子配列を指し、
value_id はターミナル子ノードに格納される。ビット分割の競合が発生しない。

**容量**: value_id は 31 bit で最大 ~2G 値。十分。

**サイズオーバーヘッド**: 各 value 付きキーにターミナルノード 8 bytes が追加される。

lexime での対応:

| 用途 | キー型 | value_id の指す先 |
|------|--------|------------------|
| 辞書 | `&str` (reading のひらがな) | オフセットテーブル経由で `&[DictEntry]` スライスを参照 |
| ローマ字 | `&[u8]` (ASCII romaji) | かな文字列テーブルのインデックス |

## API

### Label trait

```rust
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {}

impl Label for u8 {}
impl Label for char {}
```

辞書 Trie は `DoubleArray<char>` + CodeMapper、ローマ字 Trie は `DoubleArray<u8>` を使用。
CodeMapper によりラベル空間は実効 ~4000 に圧縮されるため、
素の Unicode 空間 (`char` の 0x11_0000 codepoint) の大きさは配列サイズに影響しない。

### DoubleArray

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,           // nodes[0] = sentinel、nodes[1] = root
    child_offsets: Vec<u32>,    // CSR オフセット、長さ nodes.len() + 1
    children_list: Vec<u32>,    // 子ノード index のフラット配列、長さ E
    code_map: CodeMapper,       // ラベル → 内部コード変換
    _phantom: PhantomData<L>,
}
```

### ビルド

```rust
impl<L: Label> DoubleArray<L> {
    /// ソート済みキーから構築する。
    /// 各キーに 0-indexed の value_id が自動付与される。
    ///
    /// # Panics
    /// - キーがソートされていない場合
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self;
}
```

- 入力: ソート済みキー配列。`keys[i]` の value_id は `i`
- ビルド手順:
  1. 全キーの文字頻度を集計 → CodeMapper 構築
  2. キーをリマップ済みコード列に変換 + ターミナルシンボル付与
  3. Doubly-linked free list で BASE を貪欲に配置
     (index 0/1 は sentinel/root として最初から free list 外)
  4. 各配置エッジ `(parent, child)` をフラットな edge ベクタに記録
  5. 再帰完了後、O(N + E) の count-and-scatter で `child_offsets` +
     `children_list` に flatten。`build_rec` が code 昇順で edge を
     enqueue するため、scatter が連続スロットに書き込むことで
     親内の順序が自動的に保たれる (sort 不要)
- ビルドは辞書コンパイル時 (`dictool compile`) に 1 回だけ実行

### 検索操作

```rust
impl<L: Label> DoubleArray<L> {
    /// 完全一致検索。キーが存在すれば value_id を返す。
    pub fn exact_match(&self, key: &[L]) -> Option<u32>;

    /// 共通接頭辞検索。query の各接頭辞に一致するキーを返す。
    /// ラティス構築 (Viterbi) で使用。
    pub fn common_prefix_search<'a>(&'a self, query: &'a [L])
        -> impl Iterator<Item = PrefixMatch> + 'a;

    /// 予測検索。prefix で始まる全キーを、各ノードの `children_list` スライスを
    /// DFS で走査して返す。辞書の predict / predict_ranked で使用。
    pub fn predictive_search<'a>(&'a self, prefix: &'a [L])
        -> impl Iterator<Item = SearchMatch<L>> + 'a;

    /// ノード探査。キーを辿り、値の有無と子の有無を返す。
    /// ローマ字 Trie の lookup (None/Prefix/Exact/ExactAndPrefix) で使用。
    ///
    /// O(1) で判定:
    /// 1. キーを辿って到達失敗 → None
    /// 2. ノード N に到達。`has_leaf(N)` が真ならターミナル子は
    ///    `base(N) XOR 0 == base(N)` に存在。`has_children` は CSR スライスの幅
    ///    `(child_offsets[N+1] - child_offsets[N]) > 1` で判定 (ターミナル以外に
    ///    子があるか)
    /// 3. `has_leaf(N)` が偽なら `value = None`、
    ///    `has_children = (child_offsets[N+1] - child_offsets[N]) > 0`
    pub fn probe(&self, key: &[L]) -> ProbeResult;
}

pub struct PrefixMatch {
    pub len: usize,      // 一致した接頭辞の長さ
    pub value_id: u32,
}

pub struct SearchMatch<L> {
    pub key: Vec<L>,     // 一致したキー全体 (DFS 中に構築、マッチごとにアロケーション)
    pub value_id: u32,
}

pub struct ProbeResult {
    pub value: Option<u32>,  // 値があれば value_id
    pub has_children: bool,  // 子ノードが存在するか (ターミナル子を除く)
}
```

### シリアライズ (LXTR v3)

```rust
impl<L: Label> DoubleArray<L> {
    /// 内部データの生バイト表現を返す (v3 フォーマット)。
    pub fn as_bytes(&self) -> Vec<u8>;

    /// 生バイト列から DoubleArray を復元する (コピー)。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrieError>;
}
```

**v3 バイナリフォーマット** (24 バイトヘッダ、8 バイトアライメント):

```
Offset                        Size       内容
0                             4          Magic: "LXTR"
4                             1          Version: 0x03
5                             3          予約: [0, 0, 0]
8                             4          nodes_count    (u32 LE, = N)
12                            4          children_count (u32 LE, = E)
16                            4          code_map_len   (u32 LE, バイト数)
20                            4          予約: [0, 0, 0, 0]
24                            N*8        nodes (base LE u32 + check LE u32)
24+N*8                        (N+1)*4    child_offsets (各: u32 LE)
24+N*8+(N+1)*4                E*4        children_list (各: u32 LE)
24+N*8+(N+1)*4+E*4            M          code_map データ
```

- サイズは **count 単位**。各固定セクションの byte 長は `nodes_count` と
  `children_count` から導出可能。v2 が検証していた「長さ不整合」系エラーを構造的に排除
- セクション: `nodes`, `child_offsets`, `children_list`, `code_map` の 4 つ
- 24 バイトヘッダで `nodes` は 8 バイト境界から開始。後続セクションも 4 バイト
  以上のアライメントを満たし、`Node`/`u32` 要件を保持
- 生データは `#[repr(C)]` (little-endian) で zero-copy デシリアライズ可能
- **リトルエンディアン専用** (`compile_error!` で強制)
- **v0.3 で v2 互換を破棄**。`DoubleArray::from_bytes` /
  `DoubleArrayRef::from_bytes` は version != 3 で `InvalidVersion` を
  返す。v2 で永続化したファイルは元キーから再構築が必要

#### ロード時の検証

`DoubleArray::from_bytes` / `DoubleArrayRef::from_bytes` は O(1) の
チェックのみ行う:

- Magic、version、buffer サイズの算術
- `nodes_count >= 2` (sentinel + root)
- セクションのアライメント (zero-copy 経路のみ)
- `child_offsets[0] == 0` と `child_offsets[N] == children_count`

`child_offsets` の単調性チェックは意図的に **ロード時には行わない** — O(N) の
コストで zero-copy を殺すため。不正な offset は UB を引き起こさない
(Rust のスライスは常に bounds check されるので graceful panic に留まる)。
クエリ前に厳密な検証が必要な呼び出し元は、別途 O(N) の strict validator を
実行できる

### Zero-Copy デシリアライズ

```rust
pub struct DoubleArrayRef<'a, L: Label> {
    // 各セクションは `&'a [T]` ではなく (生ポインタ, 長さ) のペアで保持。
    // `PhantomData<&'a [u8]>` が借用ライフタイムを担い、`view()` が
    // 必要時に実スライスをマテリアライズする。生ポインタを採用する
    // 理由は型の rustdoc を参照 (`DoubleArrayBacked` の自己参照
    // 構造と Stacked Borrows の相互作用)。
    nodes_ptr: *const Node,
    nodes_len: usize,
    child_offsets_ptr: *const u32,
    child_offsets_len: usize,
    children_list_ptr: *const u32,
    children_list_len: usize,
    code_map: CodeMapper,                // 常にヒープ確保 (小さいため)
    _marker: PhantomData<(&'a [u8], L)>,
}

impl<'a, L: Label> DoubleArrayRef<'a, L> {
    /// バイト列から zero-copy でデシリアライズ (v3 フォーマットのみ)。
    /// バッファは 4 バイト以上のアライメントが必要 (`Node` および `u32` アクセスのため)。
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, TrieError>;

    /// 全検索メソッドは `TrieSearch` トレイトで提供:
    /// exact_match, common_prefix_search, predictive_search, probe。

    /// 借用セクションをヒープにコピーして owned な DoubleArray に変換する。
    pub fn to_owned(&self) -> DoubleArray<L>;
}
```

- `nodes`、`child_offsets`、`children_list` は `unsafe` ポインタキャストで
  バイトバッファ内のデータを直接参照
- 安全性の根拠: `Node` が `#[repr(C)]` (8B, align 4, パディングなし)、
  実行時アライメント検証、LE ターゲット前提 (x86_64/aarch64)
- `code_map` はシリアライズ形式からの復元が必要なため常にヒープにデシリアライズ (小さいため問題なし)
- `from_bytes` は LXTR v3 フォーマット (24 バイトアライメント済みヘッダ) が必要
- `from_bytes` は最初のヘッダ parse 以降 O(1) — `nodes` / `child_offsets` /
  `children_list` を走査しない
- 典型的な使い方: ファイルを mmap して `from_bytes` に渡す
  (バッファと view を 1 つの owning 値にまとめたい場合は
  `DoubleArrayBacked::from_backing` を使う)

### 検索ロジック共有 (TrieView)

全検索メソッド (`traverse`, `exact_match`, `common_prefix_search`,
`predictive_search`, `probe`) は `TrieView<'a, L>` に一元実装:

```rust
#[derive(Clone, Copy)]
pub(crate) struct TrieView<'a, L: Label> {
    nodes: &'a [Node],
    child_offsets: &'a [u32],
    children_list: &'a [u32],
    code_map: &'a CodeMapper,
    _phantom: PhantomData<L>,
}
```

`DoubleArray` と `DoubleArrayRef` の両方が `TrieView` に委譲し、コード重複ゼロを実現。
列挙系 (`predictive_search`) は CSR の子スライスを直接 iterate し、
旧 `first_child()` 実装の O(alphabet_size) スキャンを排除。

### エラー型

```rust
pub enum TrieError {
    /// バイナリデータのマジックナンバーが不正
    InvalidMagic,
    /// バイナリデータのバージョンが非対応
    InvalidVersion,
    /// バイナリデータが切り詰められている・破損している
    TruncatedData,
    /// バイトバッファのアライメントが不正 (zero-copy アクセス不可)
    MisalignedData,
}
```

## lexime との統合

### 辞書ファイルフォーマット (LXDX、LXTR v3 セクション使用)

lexime の辞書ファイルは LXTR v3 の trie に lexime 固有のエントリテーブルを
続ける形。LXDX のバージョンは lexime リポジトリで別途管理される。
以下は v3 の trie セクション部分:

```
セクション               内容
────────────────────   ──────────────────────────
magic                  "LXDX" (4 bytes)
version                辞書フォーマットバージョン
...                    辞書固有のカウンタ
nodes                  [Node; N]             ← lexime-trie: base+check
child_offsets          [u32; N+1]            ← lexime-trie: CSR オフセット
children_list          [u32; E]              ← lexime-trie: CSR 子配列
code_map               CodeMapper            ← lexime-trie: ラベル変換
offsets                [u32; V+1]            ← lexime: value_id → entry 範囲
entries                [FlatDictEntry; M]    ← lexime: エントリ本体
```

- `FlatDictEntry`: `DictEntry` から `String` を排除したフラット表現
  (surface は別途文字列テーブルに配置し、オフセットで参照)
- **オフセットテーブル**: 1 つの reading が複数の DictEntry を持つ場合のマッピング。
  value_id `i` に対応するエントリは `entries[offsets[i]..offsets[i+1]]`

### TrieDictionary の置き換え

| 現在の API | lexime-trie 導入後 |
|-----------|-------------|
| `Trie<u8, Vec<DictEntry>>` | `DoubleArray<char>` + `Vec<DictEntry>` |
| `trie.exact_match(key)` → `Option<&Vec<DictEntry>>` | `da.exact_match(key)` → `Option<u32>` → `entries[range]` |
| `trie.common_prefix_search(query)` → iter | `da.common_prefix_search(query)` → iter |
| `trie.predictive_search(prefix)` → iter | `da.predictive_search(prefix)` → iter |
| `bincode::serialize/deserialize` | `as_bytes()` / `from_bytes()` |

`Dictionary` trait の実装は変わらない。内部のデータ構造だけが置き換わる。

### RomajiTrie の置き換え

| 現在 | lexime-trie 導入後 |
|------|-------------|
| `HashMap<u8, Node>` ツリー | `DoubleArray<u8>` |
| `lookup() → TrieLookupResult` | `probe() → ProbeResult` → `TrieLookupResult` に変換 |
| 動的に `insert` | ビルド時に `DoubleArray::build()` で構築 (static) |

```rust
// RomajiTrie::lookup の実装イメージ
pub fn lookup(&self, romaji: &str) -> TrieLookupResult {
    let result = self.da.probe(romaji.as_bytes());
    match (result.value, result.has_children) {
        (None, false) => TrieLookupResult::None,
        (None, true) => TrieLookupResult::Prefix,
        (Some(id), false) => TrieLookupResult::Exact(self.kana[id as usize].clone()),
        (Some(id), true) => TrieLookupResult::ExactAndPrefix(self.kana[id as usize].clone()),
    }
}
```

ローマ字 Trie は ASCII のみなので byte-wise (`DoubleArray<u8>`)。
CodeMapper は頻度順リマップを使用（identity 変換より配列が密になり有利）。

## クレート構成

```
lexime/
├── lexime-trie/           ← 本クレート (独立リポジトリ)
│   ├── Cargo.toml         [dependencies] なし (dev: criterion)
│   └── src/
│       ├── lib.rs         pub mod + DoubleArray + TrieError
│       ├── label.rs       Label trait + u8/char impl
│       ├── node.rs        Node (base + check, 8B)
│       ├── code_map.rs    CodeMapper (頻度順ラベル再マッピング)
│       ├── build.rs       DoubleArray::build() + CSR flatten
│       ├── search.rs      検索メソッドの TrieView 委譲
│       ├── serial.rs      as_bytes, from_bytes, HeaderV3, validate_cheap
│       ├── view.rs        TrieView — 共有検索ロジック
│       └── da_ref.rs      DoubleArrayRef — zero-copy デシリアライズ
├── engine/                ← 既存クレート (lexime-trie に依存)
│   └── Cargo.toml         trie-rs, serde, bincode を削除 → lexime-trie を追加
└── Cargo.toml             ← workspace 化
```

## 制約・非目標

- **挿入・削除の動的操作はサポートしない**。ビルド済みの不変 Trie のみ
- **圧縮 (TAIL 圧縮、MpTrie 等) は初期実装に含めない**。必要になったら追加

## 実装状況

1. **Node + Label + CodeMapper** — 基本型の定義とラベル再マッピング ✅
2. **build** — ソート済みキーから Double-Array を構築 (free list + CSR flatten) ✅
3. **exact_match** — 最も単純な検索 ✅
4. **common_prefix_search** — ラティス構築に必要 ✅
5. **predictive_search** — 予測候補に必要 (`children_list` 使用) ✅
6. **probe** — ローマ字 Trie に必要 ✅
7. **as_bytes / from_bytes** — シリアライズ (LXTR v3 フォーマット) ✅
8. **DoubleArrayRef / from_bytes** — zero-copy mmap デシリアライズ ✅
9. **v3 移行** — v2 破棄、root を `nodes[1]` に移動、siblings を CSR に置換 ✅
10. **lexime 統合** — TrieDictionary と RomajiTrie の内部を差し替え

## 移行ノート

### v0.2 → v0.3

v3 は **破壊的変更**。永続化された v2 ファイルは `TrieError::InvalidVersion` で
拒否される。元のキー集合から再ビルドが必要。

動機:

- `siblings: Vec<u32>` は、子の配置順と検索時の `first_child()` の
  発見順が一致することを暗黙の前提にしていた。両者のドリフトが
  `predictive_search` から静かにキーを脱落させる原因になっていた
  (#21 の regression 参照)
- `siblings` を `child_offsets` + `children_list` に置換することで
  順序が暗黙の emergent property ではなく格納された property になり、
  同種のバグを構造的に排除
- root を `nodes[1]` に移すことで `check == 0` が「未使用スロット」の
  一意な意味を持ち、search path の多段防衛コードを整理
- `first_child()` の alphabet スキャン除去により、50k ひらがな
  ベンチで `predictive_search` が約 47% 高速化
