// Free-function HTML-fragment converters — pure CPU, no client/network.
//
// The Rust impls live in hashiverse-lib/src/tools/plain_text_post.rs and have
// their own thorough unit tests; these tests just verify the NAPI bindings are
// wired up and pass strings through correctly.

import { describe, it, expect } from "vitest";

import {
    convertTextToHashiverseHtml,
    convertTextToHashiverseHtmlXHashtag,
    convertTextToHashiverseHtmlXMention,
    convertTextToHashiverseHtmlXUrlPreview,
} from "../index.js";

describe("convert helpers", () => {
    it("xHashtag returns the canonical element", () => {
        const out = convertTextToHashiverseHtmlXHashtag("RuStLang");
        expect(out).toContain('hashtag="rustlang"');
        expect(out).toContain('<span class="plugin-hashtag-right">RuStLang</span>');
        expect(out).toContain('<span class="plugin-hashtag-left">#</span>');
    });

    it("xMention returns a 64-hex element", () => {
        const hexId = "a".repeat(64);
        const out = convertTextToHashiverseHtmlXMention(hexId);
        expect(out).toBe(`<mention client_id="${hexId}"></mention>`);
    });

    it("xUrlPreview with image renders all sections", () => {
        const out = convertTextToHashiverseHtmlXUrlPreview(
            "Title",
            "Desc",
            "https://img.example/x.png",
            "https://example.com/path",
        );
        expect(out).toContain('<div class="plugin-urlpreview-card">');
        expect(out).toContain('<div class="plugin-urlpreview-card-image-container">');
        expect(out).toContain('<img src="https://img.example/x.png" alt="" class="plugin-urlpreview-card-image unblur-image">');
        expect(out).toContain('<div class="plugin-urlpreview-card-domain">example.com</div>');
        expect(out).toContain('<a class="plugin-urlpreview-card-title" href="https://example.com/path" rel="noopener noreferrer nofollow">Title</a>');
        expect(out).toContain('<div class="plugin-urlpreview-card-description">Desc</div>');
    });

    it("xUrlPreview without image skips the image branch", () => {
        const out = convertTextToHashiverseHtmlXUrlPreview("Title", "Desc", "", "https://example.com/");
        expect(out).not.toContain("plugin-urlpreview-card-image-container");
        expect(out).not.toContain("<img ");
        expect(out).toContain("example.com");
    });

    it("full text handles hashtag + mention + newline", () => {
        const hexId = "b".repeat(64);
        const out = convertTextToHashiverseHtml(`#Rust @${hexId}\nbye`);
        expect(out).toContain('hashtag="rust"');
        expect(out).toContain(`<mention client_id="${hexId}"></mention>`);
        expect(out).toContain("<br>");
    });
});
