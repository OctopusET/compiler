# legalize-kr-compiler

[legalize-kr/legalize-pipeline]으로 다운받은 API 응답 캐시를 git으로 바꿔주는
컴파일러입니다. API 응답 캐시는 [여기]에서 다운받으실 수 있습니다.

[legalize-kr/legalize-pipeline]: https://github.com/legalize-kr/legalize-pipeline
[여기]: https://github.com/legalize-kr/legalize-kr/discussions/8

## 사용법
```bash
legalize-kr-compiler <input_cache_dir> [-o <output_git_dir>]
```

기본 출력 경로는 `./output.git`입니다. 결과물은 bare repo이므로 내용을 보려면
clone해서 확인하면 됩니다.

```
legalize-kr-compiler ../.cache
git clone ./output.git ./legalize-kr
cd legalize-kr
```

출력 bare repo 경로를 직접 지정할 수도 있습니다.

```bash
legalize-kr-compiler ../.cache -o ./another.git
```

## 동작 방식

2-pass로 동작합니다.

1. `history/*.json`에서 `MST -> 제개정구분명` 매핑을 로드합니다.
   - 이때, `history/`가 없으면 amendment 정보 없이 `detail/`만으로 빌드합니다.
2. `detail/*.xml`의 메타데이터만 읽어 정렬용 entry를 만듭니다.
3. entry를 다음 순서로 정렬합니다.
   - `공포일자 asc`
   - `법령명 asc`
   - `공포번호 asc (numeric, missing last)`
   - `MST asc (numeric)`
4. 경로 충돌 규칙을 적용해 출력 파일 경로를 확정합니다.
5. 정렬된 순서대로 XML 본문을 다시 파싱해 Markdown을 만들고 commit을 작성합니다.

## 출력 특성

- 매 실행마다 fresh bare repo를 새로 만듭니다.
- branch는 `main`입니다.
- commit author/committer는 `legalize-kr-bot <bot@legalize.kr>`입니다.
- commit timestamp는 공포일자 기준 KST `12:00:00`입니다.
- `1970-01-01` 이전 날짜는 epoch 이전 commit을 피하기 위해 clamp합니다.

## 개발
```bash
# test
cargo test

# format
cargo fmt

# lint
cargo clippy
```

## 프로파일링
`profiling` profile은 `release` 최적화를 유지하면서 debug symbols를 포함합니다.

```bash
cargo build --profile profiling
samply record -- ./target/profiling/legalize-kr-compiler ../.cache
```
