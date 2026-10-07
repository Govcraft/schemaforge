import unittest

from check_surrealdb_graph import check_graph


class RemoteSurrealGraphTests(unittest.TestCase):
    def test_remote_client_is_accepted(self):
        self.assertTrue(check_graph("surrealdb v3.3.0|protocol-http,protocol-ws,rustls"))

    def test_engine_parser_and_empty_graph_are_rejected(self):
        remote = "surrealdb v3.3.0|protocol-http,protocol-ws,rustls"
        for extra in ("surrealdb-engine-local v3.3.0|", "surrealdb-kvs v3.3.0|"):
            self.assertFalse(check_graph(remote + "\n" + extra))
        for feature in ("kv-mem", "kv-rocksdb", "parse", "default"):
            self.assertFalse(check_graph(remote + "," + feature))
        self.assertFalse(check_graph(""))
