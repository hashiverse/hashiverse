import { supported_locales } from "../i18n/strings";

export const PUBLISHED_DATE = "2026-04-20";
export const SITE_URL = "https://www.hashiverse.com";
export const OG_IMAGE_URL = `${SITE_URL}/og-image.jpg`;
export const LOGO_URL = `${SITE_URL}/logo.png`;
export const REPO_URL = "https://github.com/hashiverse/hashiverse";

const release_tag = import.meta.env.PUBLIC_RELEASE_TAG ?? "";
const software_version: string | undefined =
    release_tag.length > 0 ? release_tag.replace(/^v/, "") : undefined;

export interface ArticleSchemaProps {
    page_type: "Article" | "TechArticle";
    headline: string;
    description: string;
    locale: string;
    url: string;
    date_modified: string;
}

export interface BreadcrumbItem {
    name: string;
    url: string;
}

export const safe_json_ld = (obj: object): string =>
    JSON.stringify(obj).replace(/</g, "\\u003c");

export const build_organization_schema = () => ({
    "@context": "https://schema.org",
    "@type": "Organization",
    "name": "Hashiverse",
    "url": SITE_URL,
    "logo": LOGO_URL,
    "image": OG_IMAGE_URL,
    "foundingDate": PUBLISHED_DATE,
    "sameAs": [REPO_URL],
});

export const build_website_schema = (description: string) => ({
    "@context": "https://schema.org",
    "@type": "WebSite",
    "name": "Hashiverse",
    "url": SITE_URL,
    "inLanguage": supported_locales,
    "description": description,
});

export const build_software_application_schema = (description: string): Record<string, unknown> => {
    const schema: Record<string, unknown> = {
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "name": "Hashiverse",
        "applicationCategory": "SocialNetworkingApplication",
        "operatingSystem": "Web, Windows, macOS, Linux",
        "url": SITE_URL,
        "image": OG_IMAGE_URL,
        "datePublished": PUBLISHED_DATE,
        "description": description,
        "offers": {
            "@type": "Offer",
            "price": "0",
            "priceCurrency": "USD",
        },
        "license": `${REPO_URL}/blob/main/LICENSE`,
        "codeRepository": REPO_URL,
        "programmingLanguage": ["Rust", "TypeScript"],
        "author": {
            "@type": "Organization",
            "name": "Hashiverse",
            "url": SITE_URL,
        },
    };
    if (software_version !== undefined) {
        schema.softwareVersion = software_version;
    }
    return schema;
};

export const build_article_schema = (props: ArticleSchemaProps) => ({
    "@context": "https://schema.org",
    "@type": props.page_type,
    "headline": props.headline,
    "description": props.description,
    "inLanguage": props.locale,
    "url": props.url,
    "datePublished": PUBLISHED_DATE,
    "dateModified": props.date_modified,
    "image": OG_IMAGE_URL,
    "isPartOf": { "@type": "WebSite", "url": SITE_URL },
    "publisher": {
        "@type": "Organization",
        "name": "Hashiverse",
        "url": SITE_URL,
        "logo": { "@type": "ImageObject", "url": LOGO_URL },
    },
});

export const build_breadcrumb_list = (items: BreadcrumbItem[]) => ({
    "@context": "https://schema.org",
    "@type": "BreadcrumbList",
    "itemListElement": items.map((item, index) => ({
        "@type": "ListItem",
        "position": index + 1,
        "name": item.name,
        "item": item.url,
    })),
});
