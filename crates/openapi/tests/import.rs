//! The importer end to end: a document in, `.http` files out, read back
//! through the parser and the project loader.

use std::fs;

use apic_httpfile::{parse_file, Diagnostic, File};
use apic_openapi::{import, ImportResult, Options};
use tempfile::TempDir;

const PETSTORE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/petstore.yaml");

/// A temporary directory with a document in it.
struct Fixture {
    dir: TempDir,
    spec: String,
}

impl Fixture {
    fn new(name: &str, text: &str) -> Fixture {
        let dir = TempDir::new().unwrap();
        let spec = dir.path().join(name).to_string_lossy().into_owned();
        fs::write(&spec, text).unwrap();
        Fixture { dir, spec }
    }

    fn yaml(text: &str) -> Fixture {
        Fixture::new("spec.yaml", text)
    }

    /// A path under the temporary directory.
    fn path(&self, parts: &[&str]) -> String {
        let mut p = self.dir.path().to_path_buf();
        for part in parts {
            p.push(part);
        }
        p.to_string_lossy().into_owned()
    }

    /// Imports the document into `<dir>/<out>`.
    fn import_to(&self, out: &str) -> Result<ImportResult, apic_openapi::Error> {
        import(&self.spec, opts(&self.path(&[out])))
    }

    fn import(&self) -> ImportResult {
        self.import_to("out").unwrap()
    }

    fn rewrite(&self, text: &str) {
        fs::write(&self.spec, text).unwrap();
    }
}

fn opts(out_dir: &str) -> Options {
    Options {
        out_dir: out_dir.to_string(),
        ..Options::default()
    }
}

fn must_read(path: &str) -> String {
    fs::read_to_string(path).unwrap()
}

fn read_all(res: &ImportResult) -> String {
    res.files.iter().map(|f| must_read(f)).collect()
}

/// The `.http` file among the files written.
fn http_file(res: &ImportResult) -> String {
    res.files
        .iter()
        .rfind(|f| f.ends_with(".http"))
        .cloned()
        .unwrap()
}

fn parse(path: &str) -> (std::sync::Arc<File>, Vec<Diagnostic>) {
    parse_file(path).unwrap()
}

fn index(s: &str, needle: &str) -> usize {
    s.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in:\n{s}"))
}

#[test]
fn import_petstore() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().to_string_lossy().into_owned();
    let res = import(PETSTORE, opts(&out)).unwrap();
    assert!(
        res.requests == 4
            && res.files.len() == 2
            && res.base_url == "https://petstore.example.com/v1",
        "{res:?}"
    );
    let s = must_read(&format!("{out}/pets.http"));
    for want in [
        "# @name list-pets",
        "GET {{baseUrl}}/pets\n",
        "    # ?limit={{limit}}  (optional)",
        "X-Trace: {{xTrace}}",
        "# @name create-pet",
        "# @assert status == 201",
        "# @description Creates a pet.",
        "Content-Type: application/json",
        r#""name": "Rex""#,
        r#""born": "{{$isoTimestamp}}""#,
        "# @name show-pet-by-id",
        "GET {{baseUrl}}/pets/{{petId}}",
    ] {
        assert!(s.contains(want), "pets.http missing {want:?}\n{s}");
    }
    let api = must_read(&format!("{out}/api.http"));
    assert!(api.contains("# @name get-health"), "api.http:\n{api}");
    let env = must_read(&format!("{out}/http-client.env.json"));
    assert!(
        env.contains(r#""baseUrl": "https://petstore.example.com/v1""#),
        "env: {env}"
    );

    // The generated files must parse cleanly and be runnable.
    let p = apic_project::load(&out).unwrap();
    for d in p.validate() {
        assert!(d.severity != "error", "validate: {d}");
    }
    assert_eq!(p.requests().len(), 4);

    // Existing files are not overwritten without --force.
    let res2 = import(PETSTORE, opts(&out)).unwrap();
    assert!(res2.skipped.len() == 2 && res2.files.is_empty(), "{res2:?}");
}

#[test]
fn import_resolves_server_variables() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info:
  title: t
  version: 1.0.0
servers:
  - url: https://{region}.example.com/{base}
    variables:
      region:
        default: us
      base:
        default: api
paths:
  /ping:
    get:
      operationId: ping
      responses:
        "200":
          description: ok
"#,
    );
    let res = f.import();
    assert_eq!(res.base_url, "https://us.example.com/api");
}

#[test]
fn import_uses_first_non_empty_server_url() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info:
  title: t
  version: 1.0.0
servers:
  - url: ""
  - url: https://api.example.com
paths:
  /ping:
    get:
      operationId: ping
      responses:
        "200":
          description: ok
"#,
    );
    let res = f.import();
    assert_eq!(res.base_url, "https://api.example.com");
}

#[test]
fn import_refs_path_params_and_json_input() {
    let f = Fixture::new(
        "spec.json",
        r##"{
  "openapi": "3.1.0",
  "info": {"title": "t", "version": "1"},
  "servers": [{"url": "https://api.example.com/v2/"}],
  "paths": {
    "/orgs/{orgId}/members": {
      "parameters": [{"$ref": "#/components/parameters/OrgId"}],
      "get": {
        "operationId": "listMembers",
        "tags": ["members"],
        "parameters": [{"name": "limit", "in": "query", "schema": {"type": "integer"}}],
        "responses": {"200": {"description": "ok", "content": {"application/json": {"schema": {"type": "array"}}}}}
      },
      "post": {
        "operationId": "addMember",
        "tags": ["members"],
        "requestBody": {"$ref": "#/components/requestBodies/Member"},
        "responses": {"201": {"description": "created"}}
      }
    }
  },
  "components": {
    "parameters": {"OrgId": {"name": "orgId", "in": "path", "required": true, "schema": {"type": "string"}}},
    "requestBodies": {"Member": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Member"}}}}},
    "schemas": {
      "Member": {
        "type": "object",
        "properties": {
          "email": {"type": "string", "format": "email"},
          "nickname": {"type": ["string", "null"]},
          "roles": {"type": "array", "items": {"$ref": "#/components/schemas/Role"}},
          "manager": {"$ref": "#/components/schemas/Member"}
        }
      },
      "Role": {"type": "string", "enum": ["admin", "member"]}
    }
  }
}"##,
    );
    let res = f.import();
    assert!(
        res.base_url == "https://api.example.com/v2" && res.requests == 2,
        "{res:?}"
    );
    let s = must_read(&f.path(&["out", "members.http"]));
    for want in [
        "# @name list-members",
        "GET {{baseUrl}}/orgs/{{orgId}}/members\n",
        "    # ?limit={{limit}}  (optional)",
        "Accept: application/json",
        "# @name add-member",
        "# @assert status == 201",
        "POST {{baseUrl}}/orgs/{{orgId}}/members\n",
        "Content-Type: application/json",
        r#""email": "user@example.com""#,
        r#""nickname": "string""#,
        "\"roles\": [\n    \"admin\"",
        r#""manager": {"#,
    ] {
        assert!(s.contains(want), "members.http missing {want:?}\n{s}");
    }
    // Property order follows the schema.
    assert!(
        index(&s, r#""email""#) < index(&s, r#""nickname""#)
            && index(&s, r#""nickname""#) < index(&s, r#""roles""#),
        "property order not preserved:\n{s}"
    );
}

#[test]
fn import_rejects_swagger2() {
    let f = Fixture::yaml("swagger: '2.0'\ninfo: {title: t, version: '1'}\npaths: {}\n");
    let err = import(&f.spec, opts(&f.path(&[]))).unwrap_err();
    assert!(err.to_string().contains("Swagger 2.0"), "got {err}");
}

#[test]
fn import_examples_and_bool_case() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /things:
    post:
      operationId: makeThing
      parameters:
        - name: X-Req
          in: header
          required: True
          schema: {type: string}
      requestBody:
        content:
          application/json:
            examples:
              summaryOnly:
                summary: no value here
              real:
                value: {"name": "from-example"}
            schema: {type: object, properties: {name: {type: string}}}
      responses:
        "201": {description: created}
"#,
    );
    f.import();
    let s = must_read(&f.path(&["out", "api.http"]));
    assert!(
        s.contains(r#""name": "from-example""#),
        "should use the first example with a value:\n{s}"
    );
    assert!(
        s.contains("\nX-Req: {{xReq}}\n"),
        "required: True should be treated as required:\n{s}"
    );
}

#[test]
fn import_explicit_null_example() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /reset:
    post:
      operationId: reset
      requestBody:
        content:
          application/json:
            example: null
      responses:
        "204": {description: done}
"#,
    );
    f.import();
    let out = must_read(&f.path(&["out", "api.http"]));
    assert!(
        out.contains("Content-Type: application/json\n\nnull\n"),
        "explicit null example should produce a null body:\n{out}"
    );
}

#[test]
fn import_schema_precedence_null_and_pointer_index() {
    let f = Fixture::yaml(
        r##"
openapi: 3.1.0
info: {title: t, version: "1"}
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                pick: {type: string, default: from-default, examples: [from-examples]}
                nothing: {type: "null"}
                first: {$ref: "#/components/schemas/Envelope/allOf/0"}
      responses:
        "200": {description: ok}
  /b:
    post:
      operationId: b
      requestBody:
        content:
          application/json:
            schema: {type: "null"}
      responses:
        "200": {description: ok}
components:
  schemas:
    Envelope:
      allOf:
        - type: object
          properties: {id: {type: integer}}
"##,
    );
    f.import();
    let s = must_read(&f.path(&["out", "api.http"]));
    for want in [
        r#""pick": "from-examples""#,
        r#""nothing": null"#,
        "\"first\": {\n    \"id\": 1",
        "# @name b\n# @assert status == 200\nPOST {{baseUrl}}/b\nContent-Type: application/json\n\nnull\n",
    ] {
        assert!(s.contains(want), "missing {want:?} in:\n{s}");
    }
}

#[test]
fn import_patterned_status_and_ref_siblings() {
    let f = Fixture::yaml(
        r##"
openapi: 3.1.0
info: {title: t, version: "1"}
paths:
  /pets:
    post:
      operationId: createPet
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Pet"
              example: {"name": "inline-wins"}
      responses:
        "2XX": {description: any success}
components:
  schemas:
    Pet:
      type: object
      properties: {name: {type: string, example: from-ref}}
"##,
    );
    f.import();
    let s = must_read(&f.path(&["out", "api.http"]));
    for want in [
        "# @assert status >= 200\n# @assert status < 300\n",
        r#""name": "inline-wins""#,
    ] {
        assert!(s.contains(want), "missing {want:?} in:\n{s}");
    }
    assert!(
        !s.contains("2XX"),
        "patterned status must not be emitted literally:\n{s}"
    );
}

#[test]
fn import_disambiguates_colliding_param_names() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /u/{user-id}:
    get:
      operationId: getU
      parameters:
        - {name: user-id, in: path, required: true, schema: {type: string}}
        - {name: user_id, in: query, required: true, schema: {type: string}}
        - {name: userId, in: header, required: true, schema: {type: string}}
      responses:
        "200": {description: ok}
"#,
    );
    f.import();
    let s = must_read(&f.path(&["out", "api.http"]));
    for want in [
        "GET {{baseUrl}}/u/{{userId}}\n",
        "?user_id={{userIdQuery}}",
        "userId: {{userIdHeader}}",
    ] {
        assert!(s.contains(want), "missing {want:?} in:\n{s}");
    }
}

#[test]
fn import_empty_server_default_and_cookies() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers:
  - url: https://api.example.com/{base}
    variables:
      base: {default: ""}
paths:
  /me:
    get:
      operationId: me
      parameters:
        - {name: session, in: cookie, required: true, schema: {type: string}}
        - {name: theme, in: cookie, schema: {type: string}}
        - {name: csrf, in: cookie, required: true, schema: {type: string}}
      responses:
        "200": {description: ok}
"#,
    );
    let res = f.import();
    assert_eq!(
        res.base_url, "https://api.example.com",
        "empty default should substitute"
    );
    let s = must_read(&f.path(&["out", "api.http"]));
    for want in [
        "Cookie: session={{session}}; csrf={{csrf}}\n",
        "# Cookie: theme={{theme}}  (optional)\n",
    ] {
        assert!(s.contains(want), "missing {want:?} in:\n{s}");
    }
}

#[test]
fn import_tag_file_collisions_optional_first_query_and_ref_siblings_30() {
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
servers:
  - url: https://api/{region}
    variables:
      region: {}
paths:
  /a:
    get:
      operationId: a
      tags: ["foo/bar"]
      parameters:
        - {name: opt, in: query, schema: {type: string}}
        - {name: req, in: query, required: true, schema: {type: string}}
      responses: {"200": {description: ok}}
  /b:
    post:
      operationId: b
      tags: ["foo-bar"]
      requestBody:
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Pet"
              example: {"name": "ignored-in-3.0"}
      responses: {"200": {description: ok}}
components:
  schemas:
    Pet: {type: object, properties: {name: {type: string, example: from-ref}}}
"##,
    );
    let err = f.import_to("out").unwrap_err();
    assert!(
        err.to_string().contains("unresolved variable {region}"),
        "a server variable without a default must stay unresolved: {err}"
    );
    f.rewrite(&must_read(&f.spec).replacen("region: {}", "region: {default: eu}", 1));
    let res = f.import();
    assert!(
        res.files.len() == 2
            && (res.files[0].ends_with("foo-bar-2.http")
                || res.files[1].ends_with("foo-bar-2.http")),
        "colliding tags must get distinct files: {:?}",
        res.files
    );
    let all = read_all(&res);
    assert!(
        all.contains("    ?req={{req}}\n    # &opt={{opt}}  (optional)\n"),
        "active query parameters must precede optional comments:\n{all}"
    );
    for file in &res.files {
        let (_, diags) = parse(file);
        assert!(diags.is_empty(), "generated file must parse: {diags:?}");
    }
    assert!(
        all.contains(r#""name": "from-ref""#) && !all.contains("ignored-in-3.0"),
        "3.0 must ignore keys next to $ref:\n{all}"
    );
}

#[test]
fn import_cycles_and_literal_ref_in_examples() {
    // A recursive anchor inside an example must not recurse forever.
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            example: &loop {"self": *loop, "$ref": "#/components/schemas/Pet", "name": "literal"}
      responses: {"200": {description: ok}}
components:
  schemas:
    Pet: {type: object, properties: {name: {type: string, example: from-schema}}}
"##,
    );
    f.import();
    let out = must_read(&f.path(&["out", "api.http"]));
    assert!(
        out.contains(r##""$ref": "#/components/schemas/Pet""##)
            && out.contains(r#""name": "literal""#)
            && !out.contains("from-schema"),
        "example data must be kept literal, including a $ref key:\n{out}"
    );
    // A cyclic $ref chain is an error, not a silently empty schema.
    f.rewrite(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
paths:
  /b:
    post:
      operationId: b
      requestBody:
        content:
          application/json:
            schema: {$ref: "#/components/schemas/A"}
      responses: {"200": {description: ok}}
components:
  schemas:
    A: {$ref: "#/components/schemas/B"}
    B: {$ref: "#/components/schemas/A"}
"##,
    );
    let err = f.import_to("out2").unwrap_err();
    assert!(
        err.to_string().contains("cyclic"),
        "cyclic $ref should be reported: {err}"
    );
    let entries = fs::read_dir(f.path(&["out2"])).unwrap().count();
    assert_eq!(
        entries, 0,
        "no files may be written when the document is broken"
    );
}

#[test]
fn import_optional_only_query_separators() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      parameters:
        - {name: first, in: query, schema: {type: string}}
        - {name: second, in: query, schema: {type: string}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("    # ?first={{first}}  (optional)\n    # &second={{second}}  (optional)\n"),
        "only the first optional parameter may use ?:\n{all}"
    );
}

#[test]
fn import_percent_encoded_ref() {
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /p:
    post:
      operationId: p
      requestBody:
        content:
          application/json:
            schema: {$ref: "#/components/schemas/User%20Profile"}
      responses: {"200": {description: ok}}
components:
  schemas:
    User Profile: {type: object, properties: {name: {type: string, example: percent}}}
"##,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains(r#""name": "percent""#),
        "a percent-encoded $ref must resolve:\n{all}"
    );
}

#[test]
fn import_all_of_merge_and_later_media_type() {
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema:
              allOf:
                - $ref: "#/components/schemas/Base"
                - type: object
                  properties: {extra: {type: string, example: more}}
              properties: {own: {type: boolean}}
      responses: {"200": {description: ok}}
  /b:
    post:
      operationId: b
      requestBody:
        content:
          text/plain: {}
          application/json:
            example: {"from": "second"}
      responses: {"200": {description: ok}}
components:
  schemas:
    Base: {type: object, properties: {id: {type: integer}}}
"##,
    );
    let res = f.import();
    let all = read_all(&res);
    for want in [
        r#""id": 1"#,
        r#""extra": "more""#,
        r#""own": true"#,
        r#""from": "second""#,
        "Content-Type: application/json",
    ] {
        assert!(all.contains(want), "missing {want}:\n{all}");
    }
    assert!(
        !all.contains("Content-Type: text/plain"),
        "a media type without a body must not win over one with an example:\n{all}"
    );
}

#[test]
fn import_without_success_response_has_no_status_assert() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      responses:
        default: {description: anything}
        "404": {description: missing}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        !all.contains("@assert status"),
        "no 2xx response declared, so no status assertion:\n{all}"
    );
}

#[test]
fn import_self_referential_ref_with_siblings_is_cyclic() {
    let f = Fixture::yaml(
        r##"
openapi: 3.1.0
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema: {$ref: "#/components/schemas/A"}
      responses: {"200": {description: ok}}
components:
  schemas:
    A: {$ref: "#/components/schemas/A", description: loops}
"##,
    );
    let err = f.import_to("out").unwrap_err();
    assert!(
        err.to_string().contains("cyclic"),
        "a self-referential $ref with siblings must be reported as cyclic: {err}"
    );
}

#[test]
fn import_media_types_are_case_insensitive() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/JSON:
            schema: {type: string, example: plain}
      responses:
        "200":
          description: ok
          content:
            application/problem+JSON: {schema: {type: object}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("Accept: application/json") && all.contains("\n\"plain\"\n"),
        "media types are case-insensitive:\n{all}"
    );
}

#[test]
fn import_multiline_summary_stays_one_heading() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      summary: |
        List things
        GET /evil
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("### List things GET /evil\n"),
        "summary must collapse to one heading line:\n{all}"
    );
    let (file, diags) = parse(&res.files[0]);
    assert!(
        diags.is_empty() && file.requests.len() == 1,
        "generated file must parse as one request: {diags:?}"
    );
}

#[test]
fn import_all_of_object_without_properties_and_external_ref() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema:
              allOf: [{type: object}]
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("\n{}\n") && !all.contains("null"),
        "an allOf of a bare object is an empty object:\n{all}"
    );
    f.rewrite(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /b:
    post:
      operationId: b
      requestBody: {$ref: "shared.yaml#/components/requestBodies/Body"}
      responses: {"200": {description: ok}}
"#,
    );
    let err = f.import_to("out2").unwrap_err();
    assert!(
        err.to_string().contains("external $ref"),
        "an external $ref must be reported, not silently dropped: {err}"
    );
}

#[test]
fn import_avoids_reserved_variable_names() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      parameters:
        - {name: baseUrl, in: query, required: true, schema: {type: string}}
        - {name: $filter, in: query, required: true, schema: {type: string}}
        - {name: 1st, in: query, required: true, schema: {type: string}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    for want in [
        "?baseUrl={{baseUrlParam}}",
        "&$filter={{filter}}",
        "&1st={{p1st}}",
    ] {
        assert!(all.contains(want), "missing {want}:\n{all}");
    }
    assert!(
        !all.contains("{{$filter}}") && !all.contains("={{baseUrl}}"),
        "reserved names leaked:\n{all}"
    );
}

#[test]
fn import_non_json_bodies() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /form:
    post:
      operationId: form
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              properties:
                user: {type: string, example: "a b"}
                count: {type: integer}
                tags: {type: array, items: {type: string, example: x}}
                id: {type: string, format: uuid}
      responses: {"200": {description: ok}}
  /plain:
    post:
      operationId: plain
      requestBody:
        content:
          text/plain:
            example: 42
      responses: {"200": {description: ok}}
  /xml:
    post:
      operationId: xml
      requestBody:
        content:
          application/xml:
            schema: {type: object, properties: {id: {type: integer}}}
      responses: {"200": {description: ok}}
  /either:
    post:
      operationId: either
      requestBody:
        content:
          application/xml:
            schema: {type: object, properties: {id: {type: integer}}}
          application/json:
            schema: {type: object, properties: {id: {type: integer}}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("Content-Type: application/x-www-form-urlencoded\n\nuser=a+b&count=1&tags=%5B%22x%22%5D&id=00000000-0000-4000-8000-000000000000\n")
            && !all.contains("%7B"),
        "form bodies are form-encoded in property order with concrete sample values:\n{all}"
    );
    let plain_part = &all[index(&all, "# @name plain")..index(&all, "# @name xml")];
    assert!(
        plain_part.contains("Content-Type: text/plain\n\n42\n"),
        "a scalar example is a body under any media type:\n{plain_part}"
    );
    let xml_part = &all[index(&all, "# @name xml")..index(&all, "# @name either")];
    assert!(
        xml_part.contains("Content-Type: application/xml") && !xml_part.contains(r#""id""#),
        "an XML body must not be rendered as JSON:\n{xml_part}"
    );
    let either_part = &all[index(&all, "# @name either")..];
    assert!(
        either_part.contains("Content-Type: application/json")
            && either_part.contains(r#""id": 1"#),
        "a JSON media type is preferred over one that cannot be rendered:\n{either_part}"
    );
}

#[test]
fn import_property_named_ref() {
    let f = Fixture::yaml(
        r#"
openapi: 3.1.0
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                $ref: {type: boolean}
                name: {type: string, example: n}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f
        .import_to("out")
        .expect("a property named $ref is data, not a reference");
    let all = must_read(&res.files[0]);
    assert!(
        all.contains(r#""$ref": true"#) && all.contains(r#""name": "n""#),
        "body must keep the $ref property:\n{all}"
    );
}

#[test]
fn import_header_with_dot_parses() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      parameters:
        - {name: X.Correlation-ID, in: header, required: true, schema: {type: string}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let (file, diags) = parse(&res.files[0]);
    assert!(
        diags.is_empty()
            && file.requests.len() == 1
            && file.requests[0].headers.len() == 1
            && file.requests[0].headers[0].name == "X.Correlation-ID",
        "generated header must parse: {diags:?} {file:?}"
    );
}

#[test]
fn import_literal_braces_and_timestamps() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /tpl:
    post:
      operationId: tpl
      requestBody:
        content:
          application/json:
            example: {"greeting": "hello {{name}}"}
      responses: {"200": {description: ok}}
  /when:
    post:
      operationId: when
      requestBody:
        content:
          application/json:
            example: {"day": 2025-01-01, "at": 2025-01-01T10:00:00Z}
      responses: {"200": {description: ok}}
  /mixed:
    post:
      operationId: mixed
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                id: {type: string, format: uuid}
                text: {type: string, example: "keep {{this}}"}
      responses: {"200": {description: ok}}
  /fake:
    post:
      operationId: fake
      requestBody:
        content:
          application/json:
            example: {"note": "{{$uuid}}"}
      responses: {"200": {description: ok}}
  /gen:
    post:
      operationId: gen
      requestBody:
        content:
          application/json:
            schema: {type: object, properties: {id: {type: string, format: uuid}}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let http = http_file(&res);
    let all = must_read(&http);
    assert!(
        all.contains("< ./api.tpl.body.json\n") && !all.contains("hello {{name}}"),
        "a body with literal braces must be referenced as a raw body file:\n{all}"
    );
    let side = must_read(&f.path(&["out", "api.tpl.body.json"]));
    assert!(
        side.contains(r#""greeting": "hello {{name}}""#) && !side.ends_with('\n'),
        "body file must hold the literal example verbatim: {side:?}"
    );
    let (file, diags) = parse(&http);
    assert!(
        diags.is_empty()
            && file.requests[0].body_file == "./api.tpl.body.json"
            && !file.requests[0].body_file_templated,
        "generated file must reference the body file verbatim: {diags:?} {:?}",
        file.requests[0]
    );
    assert!(
        all.contains(r#""day": "2025-01-01""#) && all.contains(r#""at": "2025-01-01T10:00:00Z""#),
        "timestamps keep their written form:\n{all}"
    );
    // Mixed: a literal marker forces a verbatim body, so the generated
    // placeholder becomes a fixed sample instead of an unrendered template.
    let mixed = must_read(&f.path(&["out", "api.mixed.body.json"]));
    assert!(
        !mixed.contains("{{$uuid}}")
            && mixed.contains(r#""id": "00000000-0000-4000-8000-000000000000""#)
            && mixed.contains("keep {{this}}"),
        "mixed body: {mixed}"
    );
    // Text that happens to spell a built-in placeholder is still literal.
    let fake = must_read(&f.path(&["out", "api.fake.body.json"]));
    assert!(
        fake.contains(r#""note": "{{$uuid}}""#),
        "fake placeholder body: {fake}"
    );
    assert!(
        all.contains("< ./api.fake.body.json"),
        "an example spelling a placeholder must be sent verbatim:\n{all}"
    );
    // Purely generated placeholders stay inline and rendered.
    let gen_part = &all[index(&all, "# @name gen")..];
    assert!(
        gen_part.contains(r#""id": "{{$uuid}}""#) && !gen_part.contains("api.gen.body"),
        "generated placeholders stay inline:\n{gen_part}"
    );
}

#[test]
fn import_keeps_existing_body_file_without_force() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /tpl:
    post:
      operationId: tpl
      requestBody:
        content:
          application/json:
            example: {"greeting": "hello {{name}}"}
      responses: {"200": {description: ok}}
"#,
    );
    let out = f.path(&["out"]);
    fs::create_dir_all(&out).unwrap();
    let side = f.path(&["out", "api.tpl.body.json"]);
    fs::write(&side, "edited by hand").unwrap();
    let res = import(&f.spec, opts(&out)).unwrap();
    assert_eq!(
        must_read(&side),
        "edited by hand",
        "an existing body file must be kept without --force"
    );
    assert!(
        res.skipped.len() == 1 && res.skipped[0] == side,
        "the kept file is reported as skipped: {res:?}"
    );
    import(
        &f.spec,
        Options {
            out_dir: out,
            force: true,
            ..Options::default()
        },
    )
    .unwrap();
    let got = must_read(&side);
    assert!(
        got.contains("hello {{name}}"),
        "--force rewrites the body file: {got:?}"
    );
}

#[test]
fn import_neutralises_line_breaks_in_spec_text() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    get:
      operationId: a
      tags: ["pets\n### injected\n# @auth exec rm -rf /"]
      parameters:
        - {name: "X-Trace\nInjected: yes", in: header, required: true, schema: {type: string}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    assert!(
        all.matches("\n###").count() == 1
            && !all.contains("\n# @auth")
            && !all.contains("\nInjected"),
        "spec text must not add lines to the generated file:\n{all}"
    );
    let (file, diags) = parse(&res.files[0]);
    assert!(
        diags.is_empty()
            && file.requests.len() == 1
            && file.requests[0].headers.len() == 1
            && file.requests[0].headers[0].name == "X-TraceInjectedyes",
        "generated file must hold exactly the declared request with a token header name: {diags:?} {:?}",
        file.requests
    );
    assert!(
        file.requests[0].directives.iter().all(|d| d.key != "auth"),
        "an injected directive must not survive"
    );
}

#[test]
fn import_hardens_against_hostile_spec_values() {
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  "/a\n### injected\nGET https://evil":
    post:
      operationId: a
      tags: ["@auth exec rm -rf /"]
      parameters:
        - {name: "{{secret}}", in: query, required: true, schema: {type: string}}
        - {name: "a&b", in: query, required: true, schema: {type: string}}
        - {name: "c+d", in: query, required: true, schema: {type: string}}
        - {name: "first name", in: query, required: true, schema: {type: string}}
        - {name: "x;y", in: query, required: true, schema: {type: string}}
        - {name: "x;y", in: cookie, required: true, schema: {type: string}}
        - {name: ":", in: header, required: true, schema: {type: string}}
        - {name: "#X-Hash", in: header, required: true, schema: {type: string}}
      requestBody:
        content:
          "text/plain\nX-Injected: yes; profile=\"{{secret}}\"":
            example: "first line\n### not a new request\nlast line"
      responses:
        "200\n# @auth exec rm -rf /": {description: hostile}
        "201": {description: ok}
"##,
    );
    let res = f.import();
    let http = http_file(&res);
    let all = must_read(&http);
    let (file, diags) = parse(&http);
    assert!(
        diags.is_empty() && file.requests.len() == 1,
        "exactly one request must come out: {diags:?}\n{all}"
    );
    let r = &file.requests[0];
    assert_eq!(
        r.url,
        "{{baseUrl}}/a###injectedGEThttps://evil?%7B%7Bsecret%7D%7D={{secret}}&a%26b={{ab}}&c%2Bd={{cd}}&first%20name={{firstName}}&x%3By={{xy}}",
        "path and query names are kept on one line and encoded"
    );
    assert!(
        all.contains("# @assert status == 201\n") && !all.contains("\n# @auth"),
        "only a well-formed status key becomes a directive:\n{all}"
    );
    assert!(
        all.contains("Content-Type: text/plain X-Injected: yes; profile=\"secret\"\n")
            && r.headers.len() == 3,
        "the content type stays one header line: {:?}\n{all}",
        r.headers
    );
    assert!(
        all.contains("\nX-Hash: {{xHash}}\n"),
        "a leading # would turn the header into a comment:\n{all}"
    );
    assert!(
        all.contains(r#"# skipped header parameter ":""#) && !all.contains("\n: {{"),
        "a name that sanitises to nothing is skipped with a note:\n{all}"
    );
    assert!(
        all.starts_with("# Tag \"@auth exec rm -rf /\""),
        "the preamble is labelled so a tag cannot read as a directive:\n{all}"
    );
    assert!(
        r.directives.iter().all(|d| d.key != "auth"),
        "a tag must not become a directive: {:?}",
        r.directives
    );
    assert!(
        file.vars.is_empty(),
        "no file-level variables may come from the preamble: {:?}",
        file.vars
    );
    assert!(
        all.contains("Cookie: xy={{xyCookie}}\n"),
        "cookie names are tokens:\n{all}"
    );
    assert!(
        !r.body_file.is_empty()
            && !r.body_file_templated
            && must_read(&f.path(&["out", &r.body_file])).contains("### not a new request"),
        "a body with a ### line goes to a body file: {r:?}"
    );
}

#[test]
fn import_sidecar_json_concretises_placeholders_and_null_bodies() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /split:
    post:
      operationId: split
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                id: {type: string, format: uuid}
                note: {type: string, example: "line\n### looks like a block"}
      responses: {"200": {description: ok}}
  /nul:
    post:
      operationId: nul
      requestBody:
        content:
          text/plain:
            example: null
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let http = http_file(&res);
    let all = must_read(&http);
    let (file, diags) = parse(&http);
    assert!(
        diags.is_empty() && file.requests.len() == 2,
        "two requests must come out: {diags:?}\n{all}"
    );
    // JSON encoding escapes the line break, so the ### never starts a line:
    // the body stays inline with its placeholder rendered at run time.
    let split = &file.requests[0];
    assert!(
        split.body_file.is_empty()
            && split.body.contains(r#""id": "{{$uuid}}""#)
            && split.body.contains(r"line\n### looks"),
        "a JSON body keeps its placeholder inline: {split:?}"
    );
    assert!(
        all.contains("Content-Type: text/plain\n\nnull\n"),
        "an explicit null example is a body:\n{all}"
    );
}

#[test]
fn import_roots_paths_and_neutralises_templates_in_them() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api.example.com}]
paths:
  "@evil.example/{{$secret}}/{id}":
    get:
      operationId: a
      parameters:
        - {name: id, in: path, schema: {type: string}}
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let (file, diags) = parse(&res.files[0]);
    assert!(diags.is_empty() && file.requests.len() == 1, "{diags:?}");
    assert_eq!(
        file.requests[0].url, "{{baseUrl}}/@evil.example/%7B%7B$secret%7D%7D/{{id}}",
        "the path is rooted and its template markers are encoded"
    );
}

#[test]
fn import_resolves_root_reference() {
    // `$ref: "#"` is the whole document (an empty JSON pointer), not an
    // external file; a Reference Object pointing at it must resolve.
    let f = Fixture::yaml(
        r##"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                doc: {$ref: "#"}
      responses: {"200": {description: ok}}
"##,
    );
    let res = f.import_to("out").expect("a root $ref must resolve");
    let all = must_read(&res.files[0]);
    assert!(
        all.contains("POST {{baseUrl}}/a"),
        "request missing:\n{all}"
    );
}

#[test]
fn import_keeps_numbers_exact() {
    // Numbers beyond the machine range must not be rounded, quoted or
    // dropped; quoted digits stay strings; YAML-only spellings decode.
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /a:
    post:
      operationId: a
      requestBody:
        content:
          application/json:
            example:
              big: 9223372036854775808
              huge: 18446744073709551616
              tiny: -9223372036854775809
              exp: 1e400
              frac: 0.10
              str: "12"
              hex: 0x1F
              inf: .inf
      responses: {"200": {description: ok}}
  /b:
    post:
      operationId: b
      requestBody:
        content:
          application/json:
            example: 9223372036854775808
      responses: {"200": {description: ok}}
"#,
    );
    let res = f.import();
    let all = must_read(&res.files[0]);
    for want in [
        r#""big": 9223372036854775808"#,
        r#""huge": 18446744073709551616"#,
        r#""tiny": -9223372036854775809"#,
        r#""exp": 1e400"#,
        r#""frac": 0.10"#,
        r#""str": "12""#,
        r#""hex": 31"#,
        r#""inf": ".inf""#,
        "\n9223372036854775808\n",
    ] {
        assert!(all.contains(want), "missing {want} in:\n{all}");
    }
}

#[test]
fn import_multipart_skeleton() {
    let f = Fixture::yaml(
        r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers: [{url: https://api}]
paths:
  /upload:
    post:
      operationId: uploadReport
      requestBody:
        content:
          multipart/form-data:
            schema:
              type: object
              properties:
                title: {type: string, example: "Quarterly report"}
                tags: {type: array, items: {type: string, example: q3}}
                file: {type: string, format: binary}
                cover: {type: string, format: binary}
            encoding:
              cover: {contentType: image/png}
      responses: {"201": {description: created}}
"#,
    );
    let res = f.import();
    // Two placeholders, then the .http file.
    assert!(
        res.requests == 1 && res.files.len() == 3,
        "result = {res:?}"
    );
    let all = must_read(&f.path(&["out", "api.http"]));
    let want = concat!(
        "Content-Type: multipart/form-data; boundary=WebAppBoundary\n\n",
        "--WebAppBoundary\nContent-Disposition: form-data; name=\"title\"\n\nQuarterly report\n",
        "--WebAppBoundary\nContent-Disposition: form-data; name=\"tags\"\nContent-Type: application/json\n\n[\"q3\"]\n",
        "--WebAppBoundary\nContent-Disposition: form-data; name=\"file\"; filename=\"api.upload-report.file.bin\"\nContent-Type: application/octet-stream\n\n< ./api.upload-report.file.bin\n",
        "--WebAppBoundary\nContent-Disposition: form-data; name=\"cover\"; filename=\"api.upload-report.cover.bin\"\nContent-Type: image/png\n\n< ./api.upload-report.cover.bin\n",
        "--WebAppBoundary--\n",
    );
    assert!(
        all.contains(want),
        "multipart skeleton:\n{all}\nwant:\n{want}"
    );
    for side in ["api.upload-report.file.bin", "api.upload-report.cover.bin"] {
        assert!(
            fs::metadata(f.path(&["out", side])).is_ok(),
            "placeholder {side}"
        );
    }
    // The generated project parses into parts and validates as written.
    let p = apic_project::load(f.path(&["out"])).unwrap();
    let diags = p.validate();
    assert!(diags.is_empty(), "validate: {diags:?}");
    let m = p.requests()[0].multipart().unwrap().unwrap();
    assert!(m.parts.len() == 4 && m.files() == 2, "parts = {m:?}");
}

/// The JSON of a result is part of `apic import --json`.
#[test]
fn result_json_shape() {
    let res = ImportResult {
        base_url: "https://api".to_string(),
        ..ImportResult::default()
    };
    assert_eq!(
        serde_json::to_string(&res).unwrap(),
        r#"{"files":null,"requests":0,"base_url":"https://api"}"#
    );
    let res = ImportResult {
        files: vec!["a.http".to_string()],
        requests: 2,
        base_url: "https://api".to_string(),
        env_file: "http-client.env.json".to_string(),
        skipped: vec!["b.http".to_string()],
    };
    assert_eq!(
        serde_json::to_string(&res).unwrap(),
        r#"{"files":["a.http"],"requests":2,"base_url":"https://api","env_file":"http-client.env.json","skipped":["b.http"]}"#
    );
}
