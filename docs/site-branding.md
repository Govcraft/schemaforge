# Generated-site branding

Set product identity in your project configuration:

```toml
[schema_forge.site]
name = "Acme Operations"
# Defaults to name. Set to "" to omit the page-title suffix.
title_suffix = "Acme Operations"
logo = "branding/logo.svg"
logo_on_dark = "branding/logo-white.svg"
favicon = "branding/favicon.svg"
```

Generate and verify using the same configuration:

```sh
schemaforge --config config.toml site generate -s schemas -o site
schemaforge --config config.toml site generate -s schemas -o site --check
```

`site generate` accepts matching `--name`, `--title-suffix`, `--logo`,
`--logo-on-dark`, and `--favicon` flags. Flags override configuration values.
Configured asset paths are relative to the explicit `--config` file's directory;
with automatically discovered configuration, paths are relative to the working
directory. Asset flag paths are always relative to the working directory.
Only UTF-8 SVG assets are supported. PNG, JPEG, ICO, and other extensions are
rejected with an error. Configured SVG content is copied into managed files in
`public/`, so it is included in regeneration and drift checks.

The name appears in the navigation rail, login panel, browser title, and
accessibility statement. The document-title suffix defaults to the name;
`--title-suffix ''` disables it. Theme preferences use a key derived from the
name. Renaming a product therefore starts with its default theme preference.
The package name uses a separate safe lowercase slug.

If no name is configured, the first tenant-root schema supplies it. Otherwise,
the generator uses the schema directory's parent name, falling back to
`Application`. The output directory's basename does not determine the name.
Default marks are neutral geometric placeholders. A configured `logo` supplies
the dark mark and favicon too, unless those are explicitly configured.

## Template overrides

`--templates-dir` also supports these previously vendored outputs:

- `public/logo-mark.svg.jinja`
- `public/logo-mark-white.svg.jinja`
- `public/favicon.svg.jinja`
- `src/index.css.jinja`
- `src/lib/use-document-title.ts.jinja`

Overrides can access `branding.name`. Assets explicitly selected through config
or flags take precedence over asset templates. An unreadable template override
fails generation. Overrides remain part of the expected output for `--check`.
Use an override to customize CSS or the accessibility conformance statement.

## Existing generated sites

Brand constants, CSS, marks, title helpers, the app shell, and accessibility page
are owned outputs and update on regeneration. The login page and per-entity
page shells are preserved user files. Newly scaffolded login pages import the
owned `src/lib/branding.ts`, so future branding changes require no page rewrite.

For an existing login page, update its product copy to use the exported
`SITE_NAME` and retain `/logo-mark-white.svg` as its mark. Alternatively, back up
customizations and regenerate with `--force-user-files`; this replaces **all**
preserved page shells, not just login. This one-time migration removes the old
hard-coded vendor copy without silently overwriting user customization.
