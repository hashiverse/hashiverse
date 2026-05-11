"""
Offline tests for hashiverse_client — no server required.

These exercise client creation, key management, and local storage operations
that don't need a running hashiverse network.
"""

import tempfile
import os
import pytest
from hashiverse_client import (
    HashiverseClient,
    HashiverseError,
    convert_text_to_hashiverse_html,
    convert_text_to_hashiverse_html_x_hashtag,
    convert_text_to_hashiverse_html_x_mention,
    convert_text_to_hashiverse_html_x_url_preview,
)


@pytest.fixture
def data_dir(tmp_path):
    """Provide a fresh temporary data directory for each test."""
    return str(tmp_path / "hashiverse_data")


def make_client(data_dir, key_phrase="test keyphrase", passphrase="", bootstrap_addresses=None):
    """Helper to create a client with sensible defaults for testing."""
    return HashiverseClient.create_from_keyphrase(
        key_phrase=key_phrase,
        data_dir=data_dir,
        passphrase=passphrase,
        bootstrap_addresses=bootstrap_addresses or ["127.0.0.1:19999"],
    )


# ---------------------------------------------------------------------------
# Client creation
# ---------------------------------------------------------------------------

class TestClientCreation:
    def test_create_from_keyphrase_returns_client(self, data_dir):
        client = make_client(data_dir)
        assert client is not None

    def test_client_id_is_hex_string(self, data_dir):
        client = make_client(data_dir)
        client_id = client.client_id
        assert isinstance(client_id, str)
        assert len(client_id) > 0
        # Client IDs are hex-encoded, so every character should be hex
        assert all(c in "0123456789abcdef" for c in client_id)

    def test_logged_in_true_when_keyphrase_provided(self, data_dir):
        client = make_client(data_dir, key_phrase="some keyphrase")
        assert client.logged_in is True

    def test_logged_in_false_when_keyphrase_empty(self, data_dir):
        client = make_client(data_dir, key_phrase="")
        assert client.logged_in is False

    def test_same_keyphrase_produces_same_client_id(self, data_dir):
        client_a = make_client(data_dir, key_phrase="deterministic keyphrase")
        client_id_a = client_a.client_id

        # Create a second client in a different data dir with the same keyphrase
        other_dir = data_dir + "_other"
        client_b = make_client(other_dir, key_phrase="deterministic keyphrase")
        assert client_b.client_id == client_id_a

    def test_different_keyphrases_produce_different_client_ids(self, data_dir):
        client_a = make_client(data_dir, key_phrase="keyphrase alpha")
        other_dir = data_dir + "_other"
        client_b = make_client(other_dir, key_phrase="keyphrase bravo")
        assert client_a.client_id != client_b.client_id

    def test_data_dir_is_created(self, data_dir):
        assert not os.path.exists(data_dir)
        make_client(data_dir)
        assert os.path.isdir(data_dir)


# ---------------------------------------------------------------------------
# Key storage
# ---------------------------------------------------------------------------

class TestKeyStorage:
    def test_new_client_key_is_listed(self, data_dir):
        client = make_client(data_dir, key_phrase="stored key test")
        stored_keys = client.list_stored_keys()
        assert client.client_id in stored_keys

    def test_multiple_keys_are_listed(self, data_dir):
        client_a = make_client(data_dir, key_phrase="key alpha")
        client_b = make_client(data_dir, key_phrase="key bravo")
        stored_keys = client_b.list_stored_keys()
        assert client_a.client_id in stored_keys
        assert client_b.client_id in stored_keys

    def test_create_from_stored_key(self, data_dir):
        original = make_client(data_dir, key_phrase="restore me")
        original_id = original.client_id

        restored = HashiverseClient.create_from_stored_key(
            client_id_hex=original_id,
            data_dir=data_dir,
            passphrase="",
            bootstrap_addresses=["127.0.0.1:19999"],
        )
        assert restored.client_id == original_id
        assert restored.logged_in is True

    def test_create_from_stored_key_with_passphrase(self, data_dir):
        original = make_client(data_dir, key_phrase="encrypted key", passphrase="s3cret")
        original_id = original.client_id

        restored = HashiverseClient.create_from_stored_key(
            client_id_hex=original_id,
            data_dir=data_dir,
            passphrase="s3cret",
            bootstrap_addresses=["127.0.0.1:19999"],
        )
        assert restored.client_id == original_id

    def test_delete_stored_key(self, data_dir):
        client_a = make_client(data_dir, key_phrase="keep me")
        client_b = make_client(data_dir, key_phrase="delete me")
        client_b_id = client_b.client_id

        client_b.delete_stored_key(client_b_id)
        stored_keys = client_b.list_stored_keys()
        assert client_b_id not in stored_keys
        assert client_a.client_id in stored_keys

    def test_delete_all_stored_keys(self, data_dir):
        client_a = make_client(data_dir, key_phrase="one")
        client_b = make_client(data_dir, key_phrase="two")

        client_b.delete_all_stored_keys()
        stored_keys = client_b.list_stored_keys()
        assert len(stored_keys) == 0

    def test_create_from_nonexistent_stored_key_raises(self, data_dir):
        make_client(data_dir, key_phrase="setup")
        fake_hex_id = "ab" * 32  # 64 hex chars, won't match any real key
        with pytest.raises(HashiverseError):
            HashiverseClient.create_from_stored_key(
                client_id_hex=fake_hex_id,
                data_dir=data_dir,
                passphrase="",
                bootstrap_addresses=["127.0.0.1:19999"],
            )


# ---------------------------------------------------------------------------
# Storage reset
# ---------------------------------------------------------------------------

class TestStorageReset:
    def test_reset_storage_does_not_raise(self, data_dir):
        client = make_client(data_dir)
        # Should not raise — just clears the client's SQLite storage
        client.reset_storage()

    def test_reset_storage_keys_still_exist(self, data_dir):
        """reset_storage clears client data (posts, timelines), not keys."""
        client = make_client(data_dir, key_phrase="persist after reset")
        client_id = client.client_id
        client.reset_storage()
        stored_keys = client.list_stored_keys()
        assert client_id in stored_keys


# ---------------------------------------------------------------------------
# Anonymous client (empty keyphrase)
# ---------------------------------------------------------------------------

class TestAnonymousClient:
    def test_anonymous_client_has_client_id(self, data_dir):
        client = make_client(data_dir, key_phrase="")
        assert len(client.client_id) > 0

    def test_anonymous_client_not_logged_in(self, data_dir):
        client = make_client(data_dir, key_phrase="")
        assert client.logged_in is False


# ---------------------------------------------------------------------------
# Free-function HTML-fragment converters (pure CPU, no client/network).
#
# The Rust impls live in hashiverse-lib/src/tools/plain_text_post.rs and have
# their own thorough unit tests; these tests just verify the Python bindings
# are wired up and pass strings through correctly.
# ---------------------------------------------------------------------------


class TestConvertHelpers:
    def test_x_hashtag_returns_canonical_element(self):
        out = convert_text_to_hashiverse_html_x_hashtag("RuStLang")
        assert 'hashtag="rustlang"' in out
        assert '<span class="plugin-hashtag-right">RuStLang</span>' in out
        assert '<span class="plugin-hashtag-left">#</span>' in out

    def test_x_mention_returns_64hex_element(self):
        hex_id = "a" * 64
        out = convert_text_to_hashiverse_html_x_mention(hex_id)
        assert out == f'<mention client_id="{hex_id}"></mention>'

    def test_x_url_preview_with_image(self):
        out = convert_text_to_hashiverse_html_x_url_preview(
            "Title",
            "Desc",
            "https://img.example/x.png",
            "https://example.com/path",
        )
        assert '<div class="plugin-urlpreview-card">' in out
        assert '<div class="plugin-urlpreview-card-image-container">' in out
        assert (
            '<img src="https://img.example/x.png" alt="" '
            'class="plugin-urlpreview-card-image unblur-image">'
        ) in out
        assert '<div class="plugin-urlpreview-card-domain">example.com</div>' in out
        assert (
            '<a class="plugin-urlpreview-card-title" href="https://example.com/path" '
            'rel="noopener noreferrer nofollow">Title</a>'
        ) in out
        assert '<div class="plugin-urlpreview-card-description">Desc</div>' in out

    def test_x_url_preview_without_image_skips_image_branch(self):
        out = convert_text_to_hashiverse_html_x_url_preview(
            "Title",
            "Desc",
            "",
            "https://example.com/",
        )
        assert "plugin-urlpreview-card-image-container" not in out
        assert "<img " not in out
        # Domain still rendered, just inside card-inner above the title link.
        assert "example.com" in out

    def test_full_text_handles_hashtag_mention_newline(self):
        hex_id = "b" * 64
        out = convert_text_to_hashiverse_html(f"#Rust @{hex_id}\nbye")
        assert 'hashtag="rust"' in out
        assert f'<mention client_id="{hex_id}"></mention>' in out
        assert "<br>" in out
