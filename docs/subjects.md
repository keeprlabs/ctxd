# Subjects

Subjects are paths that address context within ctxd. They use forward-slash syntax, not dotted notation.

## Rules

- Must start with `/`
- Segments contain alphanumeric characters, hyphens (`-`), underscores (`_`), and dots (`.`)
- No empty segments (`//` is invalid)
- No trailing slash (except root `/`)
- Case-sensitive

## Examples

```
/                                    # root
/work/exlo/customers/dmitry          # specific customer context
/personal/journal/2025-01-15         # dated journal entry
/projects/ctxd/decisions/001         # architectural decision record
```

## Reading

### Exact Match

`read --subject /test/hello` returns events filed at exactly `/test/hello`.

### Recursive

`read --subject /test --recursive` returns events at `/test` and all descendants:
- `/test`
- `/test/hello`
- `/test/hello/world`

Does NOT match `/testing` (different prefix).

### Glob Patterns (for capabilities)

Capability tokens use glob patterns to scope access:
- `*` matches a single path segment
- `**` matches any number of segments

```
/test/**     # matches /test, /test/a, /test/a/b/c
/test/*      # matches /test/a but NOT /test/a/b
/**          # matches everything
```
