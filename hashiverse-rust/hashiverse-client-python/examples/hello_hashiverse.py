"""
Minimal example: create a hashiverse client and print its identity.

Usage:
    cd hashiverse-rust/hashiverse-client-python
    maturin develop
    python examples/hello_hashiverse.py
"""

import tempfile
from hashiverse_client import HashiverseClient

data_dir = tempfile.mkdtemp(prefix="hashiverse_")

print("Creating client from keyphrase...")
client = HashiverseClient.create_from_keyphrase(
    key_phrase="my example keyphrase",
    data_dir=data_dir,
    passphrase="my secret passphrase",  # encrypts key files at rest (default: "" = no encryption)
    bootstrap_addresses=["127.0.0.1:443"],  # local server for testing
)

print(f"Client ID: {client.client_id}")
print(f"Logged in: {client.logged_in}")

# List stored keys (persisted to disk, survives restarts)
keys = client.list_stored_keys()
print(f"Stored keys: {keys}")

# Reconnect using the stored key (simulating a restart)
print("\nReconnecting from stored key...")
client2 = HashiverseClient.create_from_stored_key(
    client_id_hex=client.client_id,
    data_dir=data_dir,
    passphrase="my secret passphrase",  # must match the passphrase used to create the key
    bootstrap_addresses=["127.0.0.1:443"],
)
print(f"Same identity: {client.client_id == client2.client_id}")

# Read the #hashiverse timeline
print("\nReading #hashiverse timeline...")
timeline = client.get_hashtag_timeline("hashiverse")
print(f"Got {len(timeline.posts)} posts (oldest processed: {timeline.oldest_processed_time_millis})")
for post in timeline.posts:
    print(f"  [{post.client_id[:8]}...] {post.post[:80]}")

# Fetch trending hashtags
print("\nFetching trending hashtags...")
trending = client.fetch_trending_hashtags(limit=10)
for tag in trending:
    print(f"  #{tag.hashtag} ({tag.count})")

