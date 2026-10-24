import base64
import importlib.util
import json
from pathlib import Path
import unittest

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa
import jwt

spec = importlib.util.spec_from_file_location("jit", Path(__file__).with_name("jit.py"))
jit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(jit)


class JitTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        cls.private = cls.key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )

    def config(self):
        return {
            "repository": "example/queue", "appId": 12, "installationId": 34,
            "name": "build-queue", "groupId": 1, "labels": ["gate"],
        }

    def encoded(self, settings):
        return base64.b64encode(json.dumps({".runner": json.dumps(settings)}).encode()).decode()

    def test_only_ephemeral_config_leaves_the_host(self):
        calls = []
        encoded = self.encoded({"ephemeral": True})

        def post(path, token, body):
            calls.append((path, token, body))
            if len(calls) == 1:
                claims = jwt.decode(
                    token, self.key.public_key(), algorithms=["RS256"],
                    options={"verify_exp": False, "verify_iat": False},
                )
                self.assertEqual(claims, {"iat": 940, "exp": 1540, "iss": "12"})
                self.assertEqual(body, {"repositories": ["queue"], "permissions": {"administration": "write"}})
                return {"token": "installation-secret"}
            self.assertEqual(token, "installation-secret")
            self.assertEqual(body["labels"], ["self-hosted", "Linux", "X64", "gate"])
            self.assertEqual(body["work_folder"], "/var/cache/github-runner-work")
            self.assertEqual(body["runner_group_id"], 1)
            self.assertTrue(body["name"].startswith("build-queue-"))
            return {"encoded_jit_config": encoded}

        self.assertEqual(jit.mint(self.config(), self.private, post, now=1000), encoded)
        self.assertEqual([call[0] for call in calls], [
            "/app/installations/34/access_tokens",
            "/repos/example/queue/actions/runners/generate-jitconfig",
        ])
        self.assertNotIn("installation-secret", encoded)
        self.assertNotIn("PRIVATE KEY", encoded)

    def test_persistent_or_malformed_config_is_refused(self):
        for settings in [{}, {"ephemeral": False}, {"ephemeral": 1}]:
            with self.subTest(settings=settings):
                def post(path, token, body):
                    return {"token": "installation-secret", "encoded_jit_config": self.encoded(settings)}
                with self.assertRaisesRegex(RuntimeError, "non-ephemeral"):
                    jit.mint(self.config(), self.private, post, now=1000)
        with self.assertRaises(Exception):
            jit.mint(self.config(), self.private, lambda *args: {
                "token": "installation-secret", "encoded_jit_config": "not base64"
            }, now=1000)

    def test_redirect_cannot_forward_host_credentials(self):
        with self.assertRaisesRegex(RuntimeError, "redirects are refused"):
            jit.NoRedirect().redirect_request(None, None, 302, "Found", {}, "https://other.invalid")


if __name__ == "__main__":
    unittest.main()
