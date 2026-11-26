# Neonemacs Core 최적화 시도 기록 (`neonemacs-core`)

> `neonemacs-core`는 `#![no_std]` 및 `#![forbid(unsafe_code)]` 제약 아래 동작합니다.  
> 이 문서는 성능 개선 시도마다 **무엇을 바꿨고 얼마나 빨라졌는지**, 그리고 **왜 일부 시도를 반려·보류했는지**를 기록합니다.

---

## 1. Weave Tree 코어 최적화 (`H1` · `H2`)

### [수용] `H1`: 순수 CoW → `Arc::get_mut` 제자리 확장

* **개선 내용:** `ByteSlice` 버퍼를 `Arc<Vec<u8>>`로 전환하고, 유일 소유(`strong_count == 1`) 상태의 슬라이스 말단 삽입(`<= CHUNK_CAP.min(1024)`, 이음새 병합 없음) 시 `split`/`join` 없이 `buf.truncate(range.end)` 후 제자리 확장하도록 변경.
* **개선 폭:**
  * 100 KiB 16연속 타이핑: **47.71 µs → 3.97 µs** (**-92% 이상**; 첫 분할 1.92 µs, 이후 ~0.14 µs/자)
  * 스크립트 3,000회 연속 삽입: **254.9 µs** (~85 ns/회)
  * 삭제 후 재입력: **3.32 µs**, 스냅샷(`Doc::clone()`) 유지 중 편집: **1.83 µs**, 1 MiB 원거리 4점프 편집: **23.43 µs**, 64 KiB 붙여넣기: **11.99 µs**

### [수용] `H2`: 청크·슬라이스·팬아웃 상수 튜닝

* **개선 내용:** `MAX_SLICE_BYTES = 16 KiB`, `CHUNK_CAP = 4 KiB`, `MIN_FANOUT = 16`, `MAX_FANOUT = 32` 채택 및 SIMD `memchr` 기반 라인 스캔 적용.
* **결과 및 근거:**
  * 1 MiB 라인 변환: `line_to_offset` **8.82 ns**, `offset_to_line` **29.52 ns** → 별도 라인 인덱스 테이블 없이도 수십 ns 수준을 달성.
  * 최악 조건 `memchr` 스캔 스윕: 4 KiB(213 ns), **16 KiB(811 ns)**, 64 KiB(3.27 µs) → 1 µs 미만 상한과 트리 깊이 균형을 만족하는 `16 KiB` 채택.
  * `CHUNK_CAP` 이음새 병합 비용 스윕: 256 B(136.5 ns), 1 KiB(3.11 µs), **4 KiB(190.5 ns)** → 단편화 억제를 위해 `4 KiB` 채택.

---

## 2. 에디터 입력·삭제·렌더 파이프라인 최적화 (2026-11-25)

### [수용] 시도 1: `Editor::self_insert` ↔ H1 제자리 확장 경로 연결

* **문제:** `Doc::replace`는 H1 인플레이스 경로를 탔지만, 실제 에디터 타이핑(`Editor::self_insert` → `Edit::insert`)은 매 키 입력마다 `Doc::from_bytes`로 `Doc`을 새로 할당해 `Doc::apply`로 넘겼기 때문에 H1 가드가 항상 실패하고 전체 `apply_cow`(`split_at` + `join`)로 빠졌음.
* **개선 내용:**
  * `crates/neonemacs-core/src/undo.rs`: `Edit::InsertInline { offset, data: InlineBytes }` 신설(16B 이하 삽입 시 `Doc` 할당 생략).
  * `crates/neonemacs-core/src/buffer.rs`: `Buffer::apply_recorded`가 `Edit::InsertInline`을 `self.text.replace`로 직접 전달.
  * `crates/neonemacs-core/src/editor/edit.rs`: `self_insert`에서 `c.to_string().into_bytes()` 힙 할당을 `c.encode_utf8(&mut [0u8; 4])` 스택 인코딩으로 교체.
* **개선 폭 (`before` → `after`):**
  * 연속 타이핑 중 단일 키 입력(100 KiB): **4.08 µs → 1.07 µs** (**-74.3%, 약 3.8배 향상**)
  * 16연속 키 입력(100 KiB): **56.11 µs → 12.72 µs** (**-76.3%, 약 4.4배 향상**)

### [수용] 시도 2: 백스페이스/삭제 인플레이스 축소 (`try_delete_in_place`)

* **문제:** `delete_len > 0`이면 1바이트 백스페이스조차 무조건 `apply_cow`로 진입해 트리를 자르고 합치는 힙 할당을 반복함.
* **개선 내용:**
  * `crates/weave-tree/src/tree.rs` 및 `lib.rs`: 이음새 병합이 없는 소형 삭제(`delete_len <= CHUNK_CAP.min(1024)`)에서 루트-리프가 유일 소유(`Arc::get_mut`)이고 단일 슬라이스의 양 끝 부분 삭제(`delete_len < slice.range.len()`)인 경우, `slice.range`만 축소하고 `buf.truncate` 및 요약치만 갱신하도록 구현.
* **개선 폭 (`before` → `after`):**
  * `Doc::replace` 단일 삭제(100 KiB): **1.69 µs → 669.7 ns** (**-59.5%, 약 2.5배 향상**)
  * `Doc::replace` 16연속 삭제(100 KiB): **25.38 µs → 10.10 µs** (**-59.1%, 약 2.5배 향상**)
  * `Editor::delete_only` 16연속 백스페이스(100 KiB): **30.99 µs → 14.49 µs** (**-52.1%, 약 2.1배 향상**)

### [수용] 시도 4: 소형 슬라이스 생성 시 128B 여유 공간(Slack) 확보

* **문제:** `try_append_slices`가 `try_reserve_exact(chunk.len())`으로 벡터를 만들어, 새로 생긴 슬라이스에 첫 글자를 덧붙일 때마다 즉시 `realloc`이 1회 발생함.
* **개선 내용:**
  * `crates/weave-tree/src/utf8.rs`: `chunk.len() < CHUNK_CAP`인 소형 슬라이스만 `(chunk.len() + 128).min(MAX_SLICE_BYTES)`로 예약하고, 대용량 16 KiB 청크는 기존처럼 정확히 할당.
* **개선 폭 (`before` → `after`):**
  * 새 슬라이스 첫 삽입(100 B): **235.97 ns → 120.88 ns** (**-53.9%, 약 2.0배 향상**)
  * 1 MiB 일괄 로드(`doc_from_bytes_1mb`): **126.78 µs → 125.00 µs** (메모리·속도 회귀 없음)

---

## 3. 반려 및 보류된 시도 (왜 채택하지 않았는가)

### [반려 및 원복] 시도 5: 렌더 루프 `point_col` 공유 및 단일 패스 라인 경계 스캔

* **시도 내용:** `render::scroll`과 `render::frame`을 합친 `scroll_and_frame`으로 `column()` 중복 호출을 없애고, `window_cells`의 39회 `line_to_offset` 호출을 `try_for_each_slice` + `memchr_iter(b'\n')` 단일 패스로 대체함.
* **실측 결과 (`before` → `after`):**
  * 100 KiB 일반 화면(120×40): **1.09 ms → 1.09 ms** (**-0.04%, 체감 개선 없음**)
  * 20 KiB 단일 롱 라인(120×40): **592.29 µs → 710.70 µs** (**+16.8% 성능 회귀**)
* **반려 사유:**
  1. `render::frame` 전체 비용(1.09 ms)의 대부분은 `column()`(~0.5 µs)이 아니라 120×40 `Cell` 그리드(`Vec<Vec<Cell>>`)와 라인별 `Cell` 벡터 할당에 있어, `point_col`을 공유해도 전체 프레임 시간에 영향이 없었음.
  2. 단일 패스 개행 스캔은 개행이 없는 20 KiB 롱 라인(`total_lines == 1`)에서 원래 호출조차 되지 않던 `line_to_offset` 대신 전체 20 KiB를 강제 순회해 오히려 16.8% 느려졌음.

### [보류] 시도 3: `SliceNode` 내부 팬아웃 배열 인라인화 (`MAX_FANOUT = 32`)

* **보류 사유:**
  1. `neonemacs-core`는 `#![forbid(unsafe_code)]` 및 `#![no_std]`를 강제하며 `Cargo.lock`에 `smallvec`/`arrayvec`이 없음.
  2. `unsafe` 없이 `[Option<T>; 32]`로 인라인하면 1슬라이스만 담긴 Undo `Doc` 노드까지 노드당 크기가 ~56 B에서 ~1 KiB로 약 18배 비대화됨. 향후 `Cargo.toml`에 `smallvec`(32가 아닌 4~8 슬롯 수준) 도입이 승인될 때만 재검토 예정.
