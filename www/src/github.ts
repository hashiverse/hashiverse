/** GitHub source links — import in .astro frontmatter to build hyperlinks to source code. */
const gh = 'https://github.com/hashiverse/hashiverse/tree/main';
const rust = `${gh}/hashiverse-rust`;

export const github = {
    root: gh,
    lib: `${rust}/hashiverse-lib`,
    server: `${rust}/hashiverse-server`,
    serverLib: `${rust}/hashiverse-server-lib`,
    wasm: `${rust}/hashiverse-client-wasm`,
    integrationTests: `${rust}/hashiverse-integration-tests`,
    web: `${gh}/hashiverse-client-web`,
    python: `${rust}/hashiverse-client-python`,
};
