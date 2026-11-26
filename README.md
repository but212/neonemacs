# neonemacs

개인적으로 쓰기 위해 만든 경량 터미널 텍스트 에디터입니다.

Emacs 키 바인딩과 명령 흐름을 따르되 Elisp 에뮬레이션 없이 동작하며, 외부 지원이나 범용 배포보다는 개인 작업 환경에 맞춰 유지·개선하는 것을 목적으로 합니다.

## 특징

- **Functional Core / Imperative Shell**: `crates/neonemacs-core` (`#![no_std]` + `alloc`)의 순수 상태 전이와 `src/shell`의 터미널·파일·프로세스 I/O를 분리했습니다.
- **Unicode / CJK 폭 처리**: 한글, CJK 전각 문자, 결합 문자 등의 터미널 표시 폭을 계산해 정렬을 유지합니다.
- **Zero-Overhead 원자적 영속화 (Registore)**: 사전 쓰기 로그(WAL) 없이 `Doc` 슬라이스를 임시 파일에 단일 순차 스트리밍(`WAF = 1.0`)한 뒤 `sync_data`와 `rename`으로 원자적 교체를 수행합니다.
- **크래시 복구 백업 (Recovery)**: 수정된 버퍼를 유휴 상태(기본 5초)에서 `XDG_STATE_HOME`(또는 `HOME` / `USERPROFILE` 기준 `.local/state`) 아래 `neonemacs/recovery/`에 원자적 `.rec` 파일로 자동 백업합니다. `SIGHUP`/`SIGTERM` 수신 시 즉시 긴급 덤프(`emergency_dump`)를 수행하는 기능은 Unix 전용이며, Windows 등 비-Unix 환경에서는 유휴 자동 백업과 저장·삭제·정상 종료 시 정리만 동작합니다.
- **Lua 5.4 설정**: 샌드박스된 내장 Lua 런타임(`init.lua`)으로 키맵과 설정을 구성합니다.

## 빌드 및 실행

Rust `1.88` 이상(Edition 2024)이 필요합니다.

```sh
cargo build --release
```

실행 및 CLI 옵션:

```text
Usage: neonemacs [OPTION] [+LINE] [FILE]...

  -v, --version  print the version and exit
  -h, --help     print this help and exit
  --             treat the following arguments as files
  +LINE FILE     open FILE at LINE
```

## 설정

`XDG_CONFIG_HOME` (또는 `HOME` / `USERPROFILE`) 기준 설정 디렉터리의 `neonemacs/init.lua`를 읽어 적용합니다. 설정 파일이 없거나 오류가 나더라도 기본 번들 설정으로 실행됩니다.

## 테스트 및 검사

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --skip performance::
cargo test --locked --release --test core_suite performance::
```

## 문서

- [도메인 컨텍스트 및 아키텍처](docs/context.md)
- [Weave Tree 명세](docs/weave_tree_spec.md)

## 라이선스

`MIT OR Apache-2.0` ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE))
