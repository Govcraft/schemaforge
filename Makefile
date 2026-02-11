CSS_INPUT  := crates/schema-forge-acton/static/css/input.css
CSS_OUTPUT := crates/schema-forge-acton/static/css/admin.css
CSS_CONTENT := 'crates/schema-forge-acton/templates/**/*.html'

.PHONY: css css-watch

css:
	npx @tailwindcss/cli -i $(CSS_INPUT) -o $(CSS_OUTPUT) --content $(CSS_CONTENT)

css-watch:
	npx @tailwindcss/cli -i $(CSS_INPUT) -o $(CSS_OUTPUT) --content $(CSS_CONTENT) --watch
