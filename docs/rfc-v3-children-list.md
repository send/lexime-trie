# RFC: v3 children_list 形式

> ステータス: **承認済み** (2026-04-14) — Phase 1 実装の根拠文書
> 対象リリース: lexime-trie v0.3.0
> 置き換え対象: v2 形式 (`SPEC.md` §Serialization)
> 実装完了後: 本文書を `SPEC.md` / `SPEC.ja.md` に統合し、本ファイルは PR-3 で削除

## 概要

`siblings: Vec<u32>` の並列配列を廃止し、`children_list` + `child_offsets` の
フラット CSR 風エッジ表現に置き換える。root を `nodes[0]` から `nodes[1]` に
移し、`nodes[0]` を無効な sentinel として固定する。バイナリ形式は v3 に bump。

## 動機

### 問題 1: 配置順と走査順の暗黙 invariant (#21 の根本原因)

v2 では、検索時の `first_child()` が `1..alphabet_size` の code 順でスキャンし、
`build_rec` は code 順で children を配置する (`build.rs:140`)。この 2 つの順序
一致は **コード上では一切強制されていない**。#21 は placement が一時的に byte 順に
なり、walk が code 順のままで、頻度順 CodeMapper と組み合わさった条件下で
`predictive_search` から約 30% のキーが脱落したバグ。

### 問題 2: `first_child()` が O(alphabet_size)

predictive_search で各ノード展開ごとに最大 ~4000 回の XOR プローブ
(char-wise + CodeMapper)。性能コストであると同時に、**最初の子を明示的に
記録していないから発生するスキャン** である点が本質的問題。

### 問題 3: `nodes[0]` が root と未使用スロットの両方を意味する

`check == 0` が「root の子」と「未使用スロット」を同時に意味する。このせいで
`view.rs::first_child`、`view.rs::predictive_search` の 4 条件 AND
(`has_real_terminal`)、`build.rs::find_base` の `base != 0` magic check
などの防衛コードが散在している。

## 提案する設計

### データ構造

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,           // 長さ N。nodes[0] = sentinel、nodes[1] = root
    child_offsets: Vec<u32>,    // 長さ N + 1
    children_list: Vec<u32>,    // 長さ E (総エッジ数)
    code_map: CodeMapper,
    _phantom: PhantomData<L>,
}
```

ノード index `p` を親とする子の集合:
```
children of p = children_list[child_offsets[p] .. child_offsets[p+1]]
```

`siblings` は完全に削除。

### Invariant

1. `nodes[0]` は常に `Node::default()`。子としても親としても参照されない。
2. `nodes[1]` が root。親を持たない。`check` フィールドは未使用 (慣例的に 0)。
3. `child_offsets.len() == nodes.len() + 1`。
4. `child_offsets` は単調非減少。
5. `child_offsets[0] == child_offsets[1] == 0` (sentinel は子を持たない)。
6. `child_offsets[N] == children_list.len()`。
7. `children_list.len() == E` (トライの総エッジ数)。
8. 任意の `p` と `c ∈ children_list[child_offsets[p]..child_offsets[p+1]]` について:
   - `nodes[c].check() == p` (v2 と同じ CHECK invariant)
   - `base(p) XOR child_code(c) == c` (`child_code(c) = base(p) XOR c`)
9. **子の順序 invariant**: `children_list[child_offsets[p]..child_offsets[p+1]]`
   内で、子は edge label の code 昇順で並ぶ。code 0 は terminal 記号なので、
   terminal child (存在する場合) は常に先頭。
10. active parent ではない `p` (sentinel / leaf / hole) について、
    `child_offsets[p] == child_offsets[p+1]` (空レンジ)。

### オペレーションの意味論

| オペレーション | touch する配列 | 計算量 |
|-----------|----------------|------------|
| `exact_match` | `nodes` のみ | O(key 長)、8B/node hot path — **v2 から不変** |
| `common_prefix_search` | `nodes` のみ | v2 と同じ |
| `probe.has_children` | `nodes` + `child_offsets` | O(1) |
| `predictive_search` | `nodes` + `child_offsets` + `children_list` | O(結果サイズ) |

8B/node hot path の性質は維持。列挙系オペレーション (`predictive_search`,
`probe`) のみが新配列に touch する。

#### `probe.has_children`

```
let start = child_offsets[p];
let end = child_offsets[p + 1];
let n = end - start;
has_children =
    if has_leaf(p) { n > 1 }  // 先頭が terminal、その先に子があれば true
    else           { n > 0 }
```

#### `predictive_search` の子列挙

```
for &c in &children_list[child_offsets[p]..child_offsets[p+1]] {
    let child = nodes[c];
    if child.is_leaf() {
        // terminal — (key_buf.clone(), child.value_id()) を yield
    } else {
        // extension — DFS スタックに push、label は
        // code_map.reverse(base(p) XOR c) で逆算
    }
}
```

v2 の `has_real_terminal` の 2 枝分岐は消え、1 ループで `is_leaf()` による
分岐だけで terminal / extension 両方を扱える。

### バイナリ形式 (v3)

```
Offset               Size         Content
0                    4            Magic: "LXTR"
4                    1            Version: 0x03
5                    3            Reserved (zero)
8                    4            nodes_count        (u32 LE, = N)
12                   4            children_count     (u32 LE, = E)
16                   4            code_map_len       (u32 LE, bytes)
20                   4            Reserved (zero)
24                   N * 8        nodes
24 + N*8             (N+1) * 4    child_offsets
24 + N*8 + (N+1)*4   E * 4        children_list
24 + N*8 + (N+1)*4 + E*4          code_map
```

- ヘッダは 24 バイト、`nodes` は 8B alignment から開始。
- `child_offsets` の開始位置は `24 + 8N` で 8B aligned
  (→ `u32` の 4B alignment を満たす)。
- `children_list` の開始位置は `24 + 8N + 4(N+1) = 28 + 12N` で常に 4B aligned。
- `code_map` も 4B aligned な位置から開始。
- すべてのサイズは **count 単位** (ノード数・エッジ数)。v2 は byte 単位だった。

#### サイズ導出規則

```
nodes_bytes           = N * 8
child_offsets_bytes   = (N + 1) * 4
children_list_bytes   = E * 4
```

すべて `nodes_count` と `children_count` から導出可能。独立した byte-length
フィールドを排除し、v2 でガードしていた「長さ不整合」系エラーを根絶。

#### デシリアライズ時の overflow / 検証

- `nodes_count >= 2` (sentinel + root)。`0` / `1` は `TruncatedData`。
- `children_count * 4` が `usize` overflow しないこと (checked multiplication)。
- `24 + N*8 + (N+1)*4 + E*4 + code_map_len` が overflow せず buffer size 以下
  であること (trailing bytes を許容するかは実装時に決定、後述 open question)。
- `child_offsets[N] == children_count`。
- `child_offsets` の単調非減少。
- `child_offsets[0] == 0`。
- エッジごとの `nodes[c].check() == p` 検証は load 時に **行わない** (O(E)
  スキャンは zero-copy を殺す)。これらは runtime invariant で、不正データでは
  誤った結果が出るが UB にはならない (すべての read は `child_offsets[p+1] <= E`
  で bounds check される)。

### v2 互換

**破棄**。`from_bytes` / `from_bytes_ref` は version != 3 に対して
`InvalidVersion` を返す。v2 で永続化済みのファイルを持つユーザは元のキーから
rebuild する必要がある。v0.3.0 リリース時に CHANGELOG で migration note を示す。

## build アルゴリズムの変更

`build.rs` の再帰 `build_rec` を以下に再編:

1. `nodes = vec![Node::default(); initial_cap]` で初期化。
   `nodes[0]` は sentinel、`nodes[1]` は root。
2. `FreeList::new(capacity)` で index `0` と `1` を remove (両方占有)。
3. 再帰中、エッジを `Vec<(parent_idx, child_idx)>` (または per-parent の
   sub-list) に収集。同一 parent 内の children は既存の `build_rec` のソートに
   より code 昇順で生成される。
4. 再帰完了後、末尾の default ノードを trim (v2 と同じ)。最終的な `nodes.len()`
   を `N` とする。
5. 仕上げ:
   - エッジを `parent_idx` で stable sort。同一 parent 内の順序は `build_rec`
     の挿入順 (= code 順) が保たれる。
   - prefix sum で `child_offsets: Vec<u32>` (長さ `N + 1`) を計算。
   - エッジを flatten して `children_list: Vec<u32>` (長さ `E`) を生成。
6. `find_base`: `base != 0` magic check を削除。sentinel が index 0 にあるため、
   `base XOR code == 0` となる placement は free-list が最初から弾く。

### 計算量

- エッジ収集: 再帰中 O(E)
- ソート: O(E log E)、または per-parent バケット方式で O(N + E)
- Prefix sum: O(N)

ソートコストは既存の `find_base` プロービングコストと比べて bottleneck にならない。

## テスト戦略

### 維持する回帰テスト

- `predictive_search_emits_every_key_at_scale` (#21 回帰、35k キー、miri-ignored)
- `predictive_search_root_many_children`
- `predictive_search_terminal_plus_many_children`
- `predictive_search_small_terminal_and_children`

これらは変更なしで緑を維持。

### 新規テスト (Phase 1)

1. `sentinel_is_default` — build 後 / deserialize 後ともに `nodes[0] == Node::default()`
2. `child_offsets_structural` — 長さ N+1、単調、`child_offsets[0] == 0`、`child_offsets[N] == children_list.len()`
3. `child_offsets_sentinel_empty` — `child_offsets[0] == child_offsets[1] == 0`
4. `children_list_check_invariant` — 各エッジについて `nodes[child].check() == parent`
5. `child_ordering` — 各 parent のレンジ内で子が code 昇順 (`has_leaf` 時は terminal 先頭)
6. `hole_empty_range` — 既知の hole index `h` について `child_offsets[h] == child_offsets[h+1]`
7. `v2_rejected` — v2 形式の buffer は `InvalidVersion`
8. `count_overflow_rejected` — `N = u32::MAX` の細工ヘッダは `TruncatedData` (UB にならない)
9. `deserialize_rejects_non_monotonic_offsets` — 壊れた `child_offsets` は `TruncatedData`

### ベンチマーク

`benches/search.rs` を PR-2 で更新。期待:
- `predictive_search`: 計測可能な高速化 (O(alphabet_size) スキャンの除去)
- `exact_match`, `common_prefix_search`: 変化なし (同じ hot path)

## 実装計画 (Phase 1 の PR 分割)

| PR | 内容 | Format 変更 | リスク |
|----|---------|---------------|------|
| PR-1 | Root sentinel 化: root を `nodes[1]` に移動、build 調整、`base != 0` magic check と `free_list.remove(0)` のおまじない削除 | なし (v2 形式のまま、内部変更のみ) | 中 — build と全 search path に触れる |
| PR-2 | `child_offsets` + `children_list` 導入、`siblings` 撤去、v3 format bump、v2 read path 削除、benches 更新 | **v2 → v3** | 高 — 最大の変更。回帰テストが safety net |
| PR-3 | `SPEC.md` / `SPEC.ja.md` を v3 仕様で更新。本 RFC を SPEC 本文に統合し `docs/rfc-v3-children-list.md` を削除 | なし | 低 |
| PR-4 | `Cargo.toml` を `0.3.0` に bump、CHANGELOG に migration note | なし | 低 |

PR-1 は意図的に format 互換にしてある。先に landing して soak させることで
PR-2 の規模リスクを分割できる。PR-1 merge 後は `nodes[0]` は未使用だが v2 形式で
serialize され続ける (v2 ファイルで 8 バイト無駄)。PR-2 で v2 を drop するので許容。

## 決定事項 (旧 Open question、2026-04-14 に合意)

1. **PR-1 は単独リリースしない**。`v0.3-dev` ブランチに積み、PR-2 完成後にまとめて v0.3 で出す。
2. **`child_offsets` は N+1 エントリを保持する** (最終 offset を省略しない)。bounds 計算統一のため。
3. **エッジソートは `parent_idx` による stable sort**。parent 内の順序は `build_rec` 出力順 (= code 順) が維持される。
4. **`code_map_len` は bytes 単位のまま**。CodeMapper 自身の形式は別扱い、`serial.rs` / `da_ref.rs` のそのセクションを無変更で済ませる。
5. **末尾余剰バイトは許容**。v2 の挙動を維持し forward-compat を確保。

## Non-goals

- `Node` 構造体レイアウト、`CodeMapper`、`Label` トレイトの変更はしない。
- `exact_match` / `common_prefix_search` の hot path は変更しない。
- build 時の `find_base` placement heuristic は変更しない。
- 反復版 `build_rec`、`TrieSearch` トレイト化、`num_nodes()` リネーム等は
  Phase 3 の項目。

## Decision log (却下した代替案)

- **却下**: `first_child: Vec<u32>` 中間ステップ。children_list に進んだ時点で
  完全破棄になり、format bump 2 回、総工数 ~30–40% 増。
- **却下**: Node 12B 化 (`first_child` を Node 内に埋め込み)。
  `exact_match` の 8B/cache-line hot path を壊す。
- **却下**: sentinel 区別のための VALID ビット追加。有効 index 空間が 2G → 1G に縮小。
- **却下**: `children_list` を u16 の code 配列にする案。children_list の
  メモリ ~50% 削減だが、predictive_search で毎回 XOR が必要になり複雑化。
  将来の phase で再検討可能。
