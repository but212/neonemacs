# 기술 명세서: Weave Tree

> `Weave Tree`는 **요약된 불변 슬라이스 B-Tree(`Doc`)**와 **구조적 치환(`Replace`)**을 코어로 삼는 임베딩 가능 순수 함수형 텍스트 엔진이다.
>
> **설계 원칙:** 코어 내부는 트리 구조(`Doc`) 위에서 평탄화 없이 동작하며, 순차 바이트 스트림(`&[u8]`) 변환은 입·출력 경계에만 격리한다.

---

## 1. 개요 (Overview)

호스트 입력(`&[u8]` 또는 `Replace`)은 단위 경계 정합과 `split`/`join`을 거쳐 새 `Doc`(`SliceTree`)과 역연산 `Replace { insert: Doc }`(Undo 스택 저장)로 전이하며, 파일 저장과 WAL·스냅샷 직렬화는 `Doc`의 `try_for_each_slice` 제로카피 스트리밍으로 출력한다.

### 1.1. 호스트 에디터 통합 경계 (Host vs `Doc`)

`Doc`은 바이트·라인 요약과 단위 경계 무결성만 책임진다. 줄바꿈(`Lf`/`Crlf`), Undo 스택, 커서/마크/뷰포트(`point`, `mark`, `top`), 더티/에폭(epoch), 디스크·WAL I/O는 **호스트**가 소유한다.

* **경계 스냅 및 앵커 재정렬:** 무오류(infallible) 편집을 원하면 호스트가 `Doc::replace`/`Doc::apply` 전 `floor_boundary`/`ceil_boundary`로 `offset`·`offset + delete_len`을 `0..=total_bytes()` 유효 `Unit` 경계에 스냅한다(유효 경계면 불변). 코어 검증 위임 시 실패하면 `(Self, DocError)`로 원본 `Doc` 소유권을 회수한다. `D-01` 재병합(`k_left > 0 || k_right > 0`) 시 앵커가 새 UTF-8 문자 내부에 놓일 수 있으므로 전이 직후 커서·마크·뷰포트를 `floor_boundary`/`ceil_boundary`로 재정렬한다(문자 단위 이동은 엄격 `<`/`>`인 `prev_boundary`/`next_boundary` 사용).
* **UTF-8·라인 경계:** ① `D-01`은 RFC 3629 UTF-8만 `Unit::Char`, overlong·서로게이트·불완전 바이트는 `Unit::Raw`로 보존한다. ② `prev_boundary(0)`·`floor_boundary(0)`은 `0`을 반환한다. ③ `line_to_offset`은 `OutOfBounds`를 반환하므로 1-based `goto-line` 클램핑은 `D-03`(`doc.total_lines() >= 1`) 아래 `n.saturating_sub(1).min(doc.total_lines() - 1)`을 사용한다.
* **Undo 스택 연동:** 호스트는 `Doc::apply`/`Doc::replace`가 반환하는 역연산 `Replace`와 논리 편집 범위(`EditSpan`)를 `RecordedEdit`으로 함께 저장한다.
  * **Undo 바이트 예산:** 역패치 삽입부가 `CHUNK_CAP`(4 KiB) 이하인 경우, 백킹 바이트가 `CHUNK_CAP`을 초과하면 저장 전에 필요한 바이트만 복사한다. 각 편집은 논리 편집 크기(`removed + inserted`)와 역패치가 유지하는 백킹 바이트 수 중 큰 값으로 `Config.undo_limit`에 반영한다. 백킹 바이트 수는 슬라이스 범위가 아닌 전체 할당 크기를 합산하며, 공유 할당도 슬라이스마다 계산한다.
  * **연속 입력 그룹화:** 연속 `self-insert`는 `Replace`를 제자리 병합하지 않고 `amalgamating-undo-limit` 한도 내에서 동일 `Group`에 개별 `RecordedEdit`으로 누적한다.

---

## 2. 데이터 모델 (Data Model)

코어는 `std::io` 및 시그니처·필드 내 `&mut` 없는 순수 값 소유권 전달 모델을 따른다.

### 2.1. 핵심 타입 및 B-Tree 상수 (Rust)

```rust
use core::ops::Range;
use triomphe::{Arc, HeaderSlice};

/// B-Tree 노드 팬아웃 및 슬라이스 크기 상수 (H2 벤치마크 스윕 대상)
pub const MAX_FANOUT: usize = 32;
pub const MIN_FANOUT: usize = MAX_FANOUT / 2;
pub const CHUNK_CAP: usize = 4096;
/// 단일 ByteSlice 최대 크기 (line_to_offset O(N) 스캔 및 백킹 버퍼 장기 점유 방지)
pub const MAX_SLICE_BYTES: usize = 16 * 1024;

/// 텍스트의 논리적 최소 단위 (비-UTF-8 바이트는 Unit::Raw로 보존)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char(char),
    Raw(u8),
}

/// 불변 슬라이스 리프 조각 (단일 백킹 할당; 비교는 try_for_each_slice 또는 Eq 사용)
#[derive(Clone, Debug)]
pub(crate) struct ByteSlice {
    pub(crate) buf: Arc<Vec<u8>>,              // 최대 MAX_SLICE_BYTES 단위 독립 할당 (H1 Arc::get_mut 지원)
    pub(crate) range: Range<usize>,            // 항상 !range.is_empty()
    pub(crate) line_breaks: usize,             // buf[range] 내 '\n' 개수
}

/// 내부 노드 및 루트의 자식 참조 + 인라인 요약 (단일 진실 원천)
#[derive(Clone, Debug)]
pub(crate) struct ChildEntry {
    pub(crate) node: Arc<SliceNode>,
    pub(crate) bytes: usize,
    pub(crate) backing_bytes: usize,
    pub(crate) line_breaks: usize,
}

/// 요약된 불변 슬라이스 B-Tree 노드 (ChildEntry가 요약을 소유)
#[derive(Clone, Debug)]
pub(crate) enum SliceNode {
    Internal { children: Vec<ChildEntry> },
    Leaf { slices: Vec<ByteSlice> },
}

/// 순수 값 전달 기반 통합 문서 코어 (SliceTree)
/// 청크 경계와 무관하게 total_bytes() 및 바이트 스트림 동등성을 비교하는 수동 PartialEq/Eq 구현.
/// 메서드 내부 지역 커서 루프로 순회하며, 동일 바이트 오프셋에서 Arc::ptr_eq(&a.node, &b.node)인 서브트리는 O(1) 스킵.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    root: Option<ChildEntry>,
}
```

### 2.2. 연산 및 오류 타입

```rust
/// 코어 입력 및 Undo 역연산 공용 구조적 치환 연산
/// (순수 삽입: delete_len == 0, 순수 삭제: insert.total_bytes() == 0)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replace {
    pub offset: usize,
    pub delete_len: usize,
    pub insert: Doc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocError {
    /// 유효 Unit 경계 중간 분할 시도 (D-01)
    InvalidBoundary { offset: usize },
    /// 바이트 오프셋/범위 또는 메트릭 인덱스가 문서 범위를 벗어남
    OutOfBounds { offset: usize, len: usize, total: usize },
    /// 편집 결과가 문서 바이트 길이의 상한(`usize::MAX - 1`)을 초과함
    DocumentTooLarge,
}
```

---

## 3. 핵심 불변식 (Invariants)

모든 `Doc` 연산 전후 아래 불변식을 만족해야 한다(`weave_tree_proptest.rs` 내 `assert_invariants()` 명시 호출로 검증).

| ID | 불변식 | 요구조건 (Contract) |
| :--- | :--- | :--- |
| **D-01** | **단위 경계 및 정규 UTF-8 무결성** | 모든 `ByteSlice` 양 끝(`range`)과 분할·삽입·삭제·질의 구간은 유효 `Unit` 경계여야 하며(위반 시 `DocError::InvalidBoundary`, 단일 `Unit`은 `ByteSlice` 경계를 넘지 않음), RFC 3629 UTF-8만 `Unit::Char`로 인정한다(그 외 `Unit::Raw(u8)`). 편집 후 경계 인접 `Unit::Raw`(최대 3B)가 유효 UTF-8을 형성한 채 방치되거나(`k_left`/`k_right` 재병합 필수) 이음새를 쪼개는 부분 병합은 금지된다. |
| **D-02** | **트리 균형·팬아웃 및 청크 상한** | ① 모든 리프는 동일 깊이. ② 원소 수는 비루트 `[MIN_FANOUT, MAX_FANOUT]`, 루트 `Leaf` `1..=MAX_FANOUT`, 루트 `Internal` `2..=MAX_FANOUT`, 빈 문서는 `doc.root == None`. ③ 모든 `ByteSlice`는 `1 <= range.len() <= MAX_SLICE_BYTES`. |
| **D-03** | **요약 메타데이터 정합성** | `ByteSlice.line_breaks`는 `buf.slice[range]` 내 `'\n'` 개수와 일치하고 `ChildEntry.(bytes, line_breaks)`는 하위 서브트리 합과 일치한다. `backing_bytes`는 서브트리의 각 슬라이스에 대해 `buf.slice.len()`을 더한 포화 합이다. `total_bytes(doc)`·`total_lines(doc)`는 `doc.root`의 `(bytes, 1 + line_breaks)`와 같다(빈 문서: `0`, `1`). |
| **D-04** | **연산 가역성 및 상태 비의존성** | 성공한 `apply`/`replace`가 반환한 `inv: Replace`를 `next_doc.apply(inv)`에 적용하면 반드시 성공해 원문과 `doc' == doc`을 복원한다. `Replace`는 논리 오프셋과 불변 `Doc`만 보유해 트리 재구조화 후 댕글링 참조가 없다. |
| **D-05** | **관측적 순수성 및 코어 격리** | `Doc`은 `std::io` 및 공개·비공개 시그니처, 반환 타입, 영속 구조체(`Doc`·`SliceNode`·`ChildEntry`·`ByteSlice`) 필드 내 `&mut`를 전면 배제한다(`I-18` 정합; 본문 내부 `let mut` 루프와 시그니처상 `&mut` 토큰 없는 `impl FnMut(&[u8]) -> ControlFlow<B>`만 허용). `H1`의 `Arc::get_mut` 최적화 시에도 `strong_count == 1` 조건으로 외부 관측 동작이 불변이어야 한다. |

---

## 4. 핵심 알고리즘 (Operation Algorithms)

### 4.1. 텍스트 치환 (`Doc::apply`, `Doc::replace`)

`Doc::replace(self, offset, delete_len, insert: &[u8])`는 순차 바이트 입력을 수용하는 경계 어댑터이며, `Doc::apply(self, edit: Replace)`와 동일한 구조적 전이 엔진을 공유한다(`replace`는 불필요한 선행 트리 할당 없이 `insert: &[u8]`를 직접 청킹해 5단계로 합류).

1. `offset.checked_add(delete_len)` 오버플로우 또는 `offset + delete_len > doc.total_bytes()`면 `(self, DocError::OutOfBounds)`, 유효 `Unit` 경계(`D-01`)가 아니면 `(self, DocError::InvalidBoundary)`로 원본 소유권을 반환한다.
2. `delete_len == 0 && insert_total_bytes == 0`이면 조기 반환(`Replace { offset, delete_len: 0, insert: Doc::default() }`).
3. **단위 경계 이음새 사전 검사 (`D-01`):** 트리 변형 전 좌측 꼬리 `[offset.saturating_sub(3)..offset)`, 삽입 양단(최대 3B씩), 우측 머리 `[offset + delete_len..(offset + delete_len + 3).min(total)]`를 읽어 `Unit::Raw`의 UTF-8 재병합을 판별한다(모두 `>= 0x80`이므로 `line_breaks` 불변).
   * *3단 결합(`좌 + insert + 우`):* `insert_total_bytes <= 2`이고 `좌측 꼬리(k_left: 1..=3) + insert + 우측 머리(k_right: 1..=3)`가 단일 유효 UTF-8(`<= 4B`, 순수 삭제 시 `k_left >= 1 && k_right >= 1`)을 형성하면 `insert` 전체를 흡수하고 `k_left`, `k_right`, 단일 문자를 확정한다.
   * *양단 독립 결합:* 그 외에는 좌측 `좌측 꼬리(<=3B) + insert 선두(<=3B)`에서 최우측 `k_left` 기준 최장 UTF-8(`<=4B`), 잔여 `insert`가 있으면 우측 `insert 말미(<=3B) + 우측 머리(<=3B)`에서 최좌측 `k_right` 기준 최장 UTF-8(`<=4B`)을 결합한다(예: `[0xC3, 0xC3] + [0xA9]` → `[0xC3, 0xA9]`만 결합).
   * `apply`에서 흡수된 `insert` 양단은 `split`으로 절단하고, 재병합 문자(경계당 최대 4B)는 5단계 조각으로 합류시킨다.
4. **`split` 분할 및 구조적 원문 회수:** 단일 패스 스파인 누적 기반 최악 $O(\log P)$ `split(doc, pos) -> (Doc, Doc)`로 `[offset - k_left, offset + delete_len + k_right)`를 분리한다(`delete_len == 0 && k_left == 0 && k_right == 0`이면 1회 `split`). `delete_len > 0`이면 중간 트리에서 `[offset, offset + delete_len)` 구간을 **평탄화 복사 없이** `deleted_doc: Doc`으로 분리한다(`delete_len == 0`이면 `Doc::default()`).
5. **삽입 트리 구성:** `apply`는 이미 `D-01`·`D-02`를 만족하는 `edit.insert: Doc`을 그대로 사용하며(이음새 재병합 시에만 경계 4B 조각을 `join`), `replace`는 `insert: &[u8]`를 `MAX_SLICE_BYTES` 이하 유효 `Unit` 경계로 분할해 독립 백킹 할당을 가진 리프로 구성한다.
6. **`join` 재결합 및 언더플로 전파 (`D-02`):** `join(left, join(mid, right))`로 균형·팬아웃을 일괄 복원한다. `join` 하강 경로의 최하단 인접 리프에서 동일 버퍼 연속 구간(`Arc::ptr_eq && a.range.end == b.range.start`, 합 `<= MAX_SLICE_BYTES`)은 $O(1)$ 병합하고, 합 `<= CHUNK_CAP`이면 CoW 병합한다. 병합으로 리프 원소 수가 `MIN_FANOUT - 1`이 되면 스파인을 따라 형제 차용(borrow)/병합(merge)을 루트까지 전파한다(루트 `Internal` 자식 1개 시 높이 -1, 루트 `Leaf` 0개 시 `None`).
   * *필수 `proptest` 검증:* 임의 높이(1~4), 비대칭 높이 차이, `split` 직후 소규모 루트(`1..MIN_FANOUT` 리프, 2-자식 `Internal`), 비정규 UTF-8 3단 재병합(`k_left + insert + k_right`), `D-04` 역연산 왕복, 상이 청크 분할 간 `PartialEq` 동등성.
7. **역연산 `Replace` 경계 보정 (`D-01`, `D-04`):** `k_left == 0 && k_right == 0`이면 `Replace { offset, delete_len: original_insert_bytes, insert: deleted_doc }`를 반환한다. `k_left > 0 || k_right > 0`이면 `offset: offset - k_left`, `delete_len: original_insert_bytes + k_left + k_right`, `insert: join(k_left_doc, join(deleted_doc, k_right_doc))`로 확장해 반환한다.

### 4.2. 좌표 변환 (`line_to_offset`, `offset_to_line`)

* **`line_to_offset(doc, line)`:** `line >= total_lines(doc)`면 `OutOfBounds`, 빈 문서 `line == 0`은 `0`. `ChildEntry.line_breaks` 누적으로 $O(\log P)$ 하강 후 리프 `ByteSlice`(`<= MAX_SLICE_BYTES`) 내부만 스캔(말미 `\n`이면 마지막 빈 줄은 `total_bytes()`).
* **`offset_to_line(doc, offset)`:** `offset > doc.total_bytes()`면 `OutOfBounds`, 비경계면 `InvalidBoundary`. $O(\log P)$ 하강하며 이전 `ChildEntry.line_breaks`와 리프 내 `offset`까지 `\n` 수를 합산한다.

### 4.3. 양방향 제로카피 스트리밍 순회 (`try_for_each_slice`, `try_for_each_slice_rev`)

`range.start > range.end || range.end > doc.total_bytes()`면 `OutOfBounds`, 양 끝이 `Unit` 경계가 아니면 `InvalidBoundary`. 빈 범위(`offset..offset`)는 `f` 0회 호출 후 `Ok(ControlFlow::Continue(()))`. `D-05` 준수를 위해 `Iterator` 대신 `ControlFlow<B>` 기반 내부 순회로 `&buf.slice[sub_range]`를 제로카피 전달하며 `ControlFlow::Break(b)` 시 즉시 중단한다. 저장·WAL 직렬화, 뷰포트 렌더링·폭 계산, 정·역방향 검색(경계 걸침은 최소 오버랩 버퍼 유지) 모두 이 메커니즘을 사용한다.

---

## 5. 입·출력 경계 직렬화 및 내부 단편화 상한

파일·WAL 저장은 `try_for_each_slice(doc, 0..total_bytes, ...)`로 제로카피 스트리밍한다.

* **일괄 로드 (`Doc::from_bytes(bytes: &[u8]) -> Self`):** 정규화된 바이트에 대해 빈 입력은 `root: None`, 비어 있지 않으면 `MAX_SLICE_BYTES` 이하 유효 `Unit` 경계로 분할·할당 후 `MAX_FANOUT` 상향식 빌더로 $O(N)$ 구성한다(2개 이상 노드 중 마지막 노드가 `MIN_FANOUT` 미만이면 직전 형제와 `[MIN_FANOUT, MAX_FANOUT]` 범위로 재분배).
* **스트리밍 로드:** `EolStreamDecoder::try_push_chunk`와 `try_finish`로 CRLF 정규화 및 문서 구성을 수행한다. 버퍼·슬라이스·노드 할당 실패는 셸의 파일 읽기 오류로 전달되며 부분 문서는 노출하지 않는다.
* **내부 단편화 상한:** 상한 `MAX_SLICE_BYTES`(16KB)와 `CHUNK_CAP`(4KB) 이하 병합으로 삭제 후 구버전 버퍼가 즉시 해제된다. 교대 삭제 최악 패턴(`> CHUNK_CAP / 2` 조각 잔존)의 이론상 내부 증폭은 ~32배이며, 필요 시 공개 API 변경 없이 `join`에 활용률 임계치(`range.len() < buf.slice.len() / 4`) 기준 복사를 추가해 상한을 4배로 제한할 수 있다.

---

## 6. 인터페이스 명세 (Core API)

```rust
impl Doc {
    /// 바이트 스트림 입력 경계 (빈 입력: root: None, 대용량: MAX_SLICE_BYTES 단위 독립 백킹 할당)
    pub fn from_bytes(bytes: &[u8]) -> Self;

    /// 구조적 순수 전이: Doc 소비 후 새 Doc과 역연산 Replace 반환 (실패 시 원본 Doc과 DocError 반환)
    pub fn apply(self, edit: Replace) -> Result<(Self, Replace), (Self, DocError)>;

    /// 순차 바이트 편집 경계 어댑터 (사전 Doc 할당 없이 &[u8] 전달; 실패 시 원본 Doc 반환)
    pub fn replace(self, offset: usize, delete_len: usize, insert: &[u8]) -> Result<(Self, Replace), (Self, DocError)>;

    /// 최악 O(log P) CoW 부분 문서 추출 (OutOfBounds / InvalidBoundary)
    pub fn slice(&self, range: Range<usize>) -> Result<Self, DocError>;

    /// 0-based 줄 번호 <-> 바이트 오프셋 (OutOfBounds / InvalidBoundary)
    pub fn line_to_offset(&self, line: usize) -> Result<usize, DocError>;
    pub fn offset_to_line(&self, offset: usize) -> Result<usize, DocError>;

    /// 지정 범위 순/역방향 제로카피 스트리밍 출력 경계 (OutOfBounds / InvalidBoundary; D-05 준수)
    pub fn try_for_each_slice<B>(
        &self,
        range: Range<usize>,
        f: impl FnMut(&[u8]) -> core::ops::ControlFlow<B>,
    ) -> Result<core::ops::ControlFlow<B>, DocError>;
    pub fn try_for_each_slice_rev<B>(
        &self,
        range: Range<usize>,
        f: impl FnMut(&[u8]) -> core::ops::ControlFlow<B>,
    ) -> Result<core::ops::ControlFlow<B>, DocError>;

    /// 단위 경계 스냅·이동 및 단발 디코딩 (D-01에 의해 단일 Unit은 ByteSlice 경계를 넘지 않아 단일 슬라이스 내부만 디코딩)
    /// - floor_boundary: total_bytes() 클램프 후 offset 이하 최대 유효 Unit 경계 (멱등)
    /// - ceil_boundary: offset >= total_bytes()면 total_bytes(), 아니면 offset 이상 최소 유효 Unit 경계 (멱등)
    /// - prev_boundary: total_bytes() 클램프 후 0이면 0, 아니면 offset 미만 최대 경계 (후진 이동)
    /// - next_boundary: offset >= total_bytes()면 total_bytes(), 아니면 offset 초과 최소 경계 (전진 이동)
    /// - decode_at: offset >= total_bytes() 또는 비경계면 None (표준 UTF-8만 Char, 그 외 Raw)
    pub fn floor_boundary(&self, offset: usize) -> usize;
    pub fn ceil_boundary(&self, offset: usize) -> usize;
    pub fn prev_boundary(&self, offset: usize) -> usize;
    pub fn next_boundary(&self, offset: usize) -> usize;
    pub fn decode_at(&self, offset: usize) -> Option<(Unit, usize)>;

    /// 원시 바이트·총량·경계 질의
    pub fn byte_at(&self, offset: usize) -> Option<u8>;
    pub fn total_bytes(&self) -> usize;
    pub fn total_lines(&self) -> usize;
    pub fn is_boundary(&self, offset: usize) -> bool;
}
```

---

## 7. 가설 검증 결과 (Hypotheses & Benchmarks)

* **`H1` (순수 CoW 기준선 vs `Arc::get_mut` 내부 최적화 — 검증 완료):** `Arc<Vec<u8>>` 전환 및 `strong_count == 1` 말단 정렬(`buf.truncate(range.end)` 후 `range.len() <= MAX_SLICE_BYTES`) 제자리 확장 + 스택 이음새 분석을 도입해 `criterion` 벤치마크([weave_tree_bench.rs](file:///c:/Users/redog/Desktop/SJI/project/neonemacs/crates/weave-tree/benches/weave_tree_bench.rs))로 실측 검증하였다.
  * *실측 결과:* 100 KiB 16연속 타이핑 **47.71 µs → 3.97 µs**(-92% 이상, 첫 분할 1.92 µs + 후속 제자리 확장 ~0.14 µs/자), 스크립트 3,000회 연속 삽입 **254.9 µs**(~85 ns/회), 삭제 후 재입력 **3.32 µs**, `Doc::clone()` 스냅샷 유지 중 편집 **1.83 µs**, 1 MiB 원거리 4점프 편집 **23.43 µs**, 64 KiB 붙여넣기 **11.99 µs**.
* **`H2` (`CHUNK_CAP`, `MAX_SLICE_BYTES`, 팬아웃 — 검증 완료):** `MAX_SLICE_BYTES = 16` KiB, `CHUNK_CAP = 4` KiB, `MIN_FANOUT = 16`, `MAX_FANOUT = 32`에서 1 MiB `line_to_offset` **8.82 ns**, `offset_to_line` **29.52 ns**. 최악 조건(`memchr` 슬라이스 말단 스캔) 스윕: 4 KiB **213 ns**, 16 KiB **811 ns**, 64 KiB **3.27 µs**(`16 KiB`가 1 µs 미만 상한과 트리 깊이 균형을 만족). `CHUNK_CAP` CoW 이음새 병합 할당·복사 비용 스윕: 256 B **136.5 ns**, 1 KiB **3.11 µs**, 4 KiB **190.5 ns**로, 단편화 억제와 병합 비용 측면에서 `4 KiB`를 채택했다.

---

## 8. 구현 현황 (Implementation Status)

* **구현 위치:** `crates/weave-tree/src/` (`lib.rs`, `summary.rs`, `cursor.rs`, `utf8.rs`, `tree.rs`), `crates/neonemacs-core/src/lib.rs`에서 `Doc`, `DocBuilder`, `Replace`, `EditorDocExt`, `Unit`, `DocError` 노출.
* **통합 상태:** 기존 갭버퍼(`text.rs`) 및 호환 심(`len`, `is_empty`, `snap_point`, `bytes`, `to_bytes`, `line_start`, `line_end`, `nth_line_start`, `count_lines`, `Editor::delete_range`, `Eol::encode`) 완전 제거. `Buffer`, `UndoLog`(`Edit::Replace(Replace)` 역패치 CoW 저장), `KillRing`(`Vec<Doc>` + `Doc::slice`), `Editor::apply`, `syntax::highlight_line(&Doc, Range)`(단일 슬라이스 무복사), `render`, `commands`, `search`, `width`, 및 `src/shell/`(`file::save_doc`, `wal::write_wal_doc`, `wal::write_scratch_doc`)이 모두 네이티브 `weave_tree::WeaveTree` 기반 `Doc`·`Replace` 및 `try_for_each_slice` 스트리밍으로 전환됨(`assert_invariants()`는 핫패스 `O(N)` 방지를 위해 `weave_tree_proptest.rs`에서 명시 호출).
* **구현 상태:** 순수 CoW 기준선 위에 단일 패스 $O(\log P)$ `split_at`, `H1` `Arc<Vec<u8>>` 기반 `Arc::get_mut` 제자리 확장(`<= CHUNK_CAP.min(1024)` 무이음새 순수 삽입), 스택 기반 이음새 검사가 적용됨(`criterion` 벤치마크 스위트 `crates/weave-tree/benches/weave_tree_bench.rs` 포함). `MAX_SLICE_BYTES = 16 KiB` + SIMD `memchr` 기반 `line_to_offset`/`offset_to_line`은 실측 수십 ns 수준이므로 별도 라인 인덱스 테이블 없이 유지(`H2`).
* **검증:** `#![forbid(unsafe_code)]`, `I-14`·`I-18`(`crates/neonemacs-core/tests/purity.rs`), `D-01`~`D-05` 단위·`proptest` 스위트(`crates/neonemacs-core/tests/weave_tree_proptest.rs`), 워크스페이스 전체 테스트(`cargo test --workspace`) 및 `cargo clippy --workspace --all-targets -- -D warnings` 통과.
