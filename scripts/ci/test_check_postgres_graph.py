"""Regression coverage for real backend and extension compilation boundaries."""

import unittest

from check_postgres_graph import check_graph


def graph(extensions=False):
    features = ",oauth,sse" if extensions else ""
    return "\n".join((
        f"schema-forge-cli v0.49.1 (/workspace/cli)|postgres,server{features}",
        f"schema-forge-acton v0.47.1 (/workspace/acton)|postgres{features}",
        "schema-forge-postgres v0.19.0 (/workspace/postgres)|",
        f"acton-service v0.46.0|database,http{features}",
        "[build-dependencies]",
        "prost-build v0.14.0|default",
        "[dev-dependencies]",
        "tokio v1.0.0|full (*)",
    ))


class PostgresGraphTests(unittest.TestCase):
    def test_enabled_and_disabled_graphs_pass(self):
        self.assertEqual(check_graph(graph(), False), [])
        self.assertEqual(check_graph(graph(True), True), [])

    def test_embedded_engine_cannot_reenter_either_graph(self):
        for extensions in (False, True):
            for name in ("surrealdb", "surrealdb-core", "schema-forge-surrealdb"):
                with self.subTest(extensions=extensions, name=name):
                    self.assertTrue(check_graph(graph(extensions) + f"\n{name} v3.0.0|kv-mem", extensions))

    def test_test_engine_feature_without_dependency_is_rejected(self):
        for feature in ("surrealdb", "test-surrealdb"):
            self.assertTrue(check_graph(graph() + f"\nschema-forge-acton v0.47.1|{feature}", False))

    def test_feature_unification_cannot_enable_extensions_indirectly(self):
        for package in ("schema-forge-cli", "schema-forge-acton", "acton-service"):
            for feature in ("oauth", "sse", "graphql"):
                with self.subTest(package=package, feature=feature):
                    self.assertTrue(check_graph(graph() + f"\n{package} v0.1.0|{feature} (*)", False))

    def test_enabled_graph_must_exercise_both_extensions(self):
        for feature in ("oauth", "sse"):
            self.assertTrue(check_graph(graph(True).replace("," + feature, ""), True))

    def test_missing_consumers_or_backend_feature_fail_closed(self):
        for name in ("schema-forge-cli", "schema-forge-acton", "schema-forge-postgres", "acton-service"):
            with self.subTest(name=name):
                text = "\n".join(line for line in graph().splitlines() if not line.startswith(name + " "))
                self.assertTrue(check_graph(text, False))
        self.assertTrue(check_graph(graph().replace("postgres,server", "server"), False))

    def test_empty_or_invalid_output_fails(self):
        self.assertTrue(check_graph("", False))
        with self.assertRaises(ValueError):
            check_graph("cargo tree failed", False)


if __name__ == "__main__":
    unittest.main()
