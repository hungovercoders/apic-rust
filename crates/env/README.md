# apic-env

Loads `http-client.env.json`, `http-client.private.env.json` and `.env` from
a project root.

## API

| Item | What it does |
| --- | --- |
| `load(root: &Path) -> Result<Environments, Error>` | Reads the three files; missing files are fine. `Error(String)` names the file |
| `PUBLIC_FILE`, `PRIVATE_FILE`, `DOT_ENV_FILE` | The file names |
| `Environments.public`, `.private` | `BTreeMap<env, BTreeMap<name, value>>`, `$shared` included as written |
| `Environments.dot_env` | `BTreeMap<name, value>` |
| `Environments.found` | The files that were present, in load order |
| `names()` | Environment names from both JSON files, sorted, without `$shared` |
| `has(name)` | Declared in either JSON file |
| `public_vars(env)`, `private_vars(env)` | The environment's variables merged over `$shared` (a fresh map) |
| `ssl(env) -> Option<&SslConfig>` | The `SSLConfiguration` block: the environment's, else `$shared`'s. `SslConfig { cert_file, key_file, has_passphrase, verify_host: Option<bool> }` |
| `auth(env) -> BTreeMap<String, AuthConfig>` | `Security.Auth` entries in effect: `$shared` then the environment, public file then private, field by field. `AuthConfig { name, fields: AuthFields, files }` |
| `auth_names()` | Every auth name any environment declares, sorted |
| `all_auth()` | `file -> env -> name -> fields`, for validation |
| `AuthFields` | `serde_json::Map<String, Value>`: the fields as written (numbers are JSON numbers; read them with `as_f64()`) |
| `dotenv::parse(&[u8]) -> Result<BTreeMap<String, String>, String>` | The `.env` parser on its own |

## Notes

- Non-string JSON values become text: `8080`, `1.5`, `true`, `""` for null,
  compact JSON for arrays and objects (keys sorted, `<>&` escaped). Numbers
  go through a 64-bit float, so `9007199254740993` reads `9007199254740992`.
- `.env` follows the dotenv rules: comments, `export`, `KEY: value`, single
  quotes (literal), double quotes (`\n`, `\r`, `$VAR`/`${VAR}` expansion,
  `\$`), inline ` # comments`, multi-line quoted values.
  `testdata/dotenv_reference.json` holds recorded inputs and outcomes.
- A `.env` that is not valid UTF-8 is read with U+FFFD in place of the bad
  bytes (the original worked on bytes).
- JSON syntax errors read as before (`http-client.env.json: invalid
  character '}' looking for beginning of value`).
