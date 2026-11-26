# apic-assert

Parses and evaluates `# @assert` expressions: `<selector> <op> <value>`,
`exists` / `not exists`, predicates (`isInteger`, `not isEmpty`, ...),
`<selector> length <op> <n>` and `<selector> matchesSchema <file>`.

## API

| Item | What it does |
| --- | --- |
| `parse(expr) -> Result<Expr, String>` | Splits an expression; the error is the full message (`assert "...": ...`) |
| `Expr { selector, op, value, length }` | `unary()` (takes no value), `display(expected)` (how it reads in results) |
| `eval(&Expr, expected, &Response) -> Result` | Evaluates against an `apic_selector::Response`; `expected` is the rendered right-hand side |
| `eval_with(&Expr, expected, &Response, Options) -> Result` | The same with a schema loader for `matchesSchema` |
| `Result { expr, pass, actual, expected, error }` | `Serialize`: keys in that order, the last three left out when empty |
| `redact(expr, mask) -> String` | Masks the value, keeps selector and operator |
| `OPERATORS`, `PREDICATES` | The operator and predicate names, in the order messages list them |
| `parse_number(s) -> Option<Number>` | The exact decimal an assertion treats as numeric (at most 4096 digits, exponent within ±4096); `Number` is `apic_selector::Number` |
| `Options<'a> { schema: Option<&'a SchemaLoader<'a>> }` | `Copy`; `Options::default()` refuses `matchesSchema` |
| `SchemaLoader<'a>` | `dyn Fn(&str) -> Result<Arc<Resolved>, String> + 'a` |
| `schemas(read) -> impl Fn(&str) -> Result<Arc<Resolved>, String>` | A loader over `read: Fn(&str) -> Result<Vec<u8>, String>` that reads, parses and resolves each file once (errors are cached too); not for use across threads |
| `Resolved` | A resolved JSON Schema: `Resolved::new(&serde_json::Value)`, `validate(&serde_json::Value) -> Result<(), String>` |

Typical use by the runner:

```rust
let loader = apic_assert::schemas(|path| read_project_file(path));
let opts = apic_assert::Options { schema: Some(&loader) };
let result = apic_assert::eval_with(&expr, &expected, &response, opts);
```

## JSON Schema

`src/schema/` is a validator for draft 2020-12 and draft-07 written for this
crate: every validation keyword, `$ref`/`$anchor`/`$dynamicRef` within the
one file, `$id` scopes. A `$ref` to another file is refused (`loading ...:
cannot resolve remote schemas`). Failures read as they always have
(`validating root: validating /properties/id: type: 1 has type "string",
want "integer"`); `testdata/schema_reference.json` holds 280 recorded
outcomes that `schema_tests.rs` replays.

## Notes

- Where several properties fail at once, the first in key order is reported
  (the original picked one at random); `unexpected additional properties`
  lists them sorted.
- A keyword of the wrong type in a schema is reported as `... of type
  jsonschema.Schema` where the original printed a very long type name.
- A schema that refers to itself without consuming the instance fails with
  `the schema refers to itself without end` instead of overflowing the
  stack. Validation runs on a thread of its own with a large stack.
- `minLength: 1e2` is accepted as 100.
- Regex failures are worded by the `regex` crate (see `apic-selector`).
