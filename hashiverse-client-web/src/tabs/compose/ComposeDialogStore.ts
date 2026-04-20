export interface ComposeOpenContext {
	initial_html?: string;
	on_posted?: () => void | Promise<void>;
	share_image_blob?: Blob;
	share_youtube_url?: string;
	share_url?: string;
}

type OpenFn = (context?: ComposeOpenContext) => void | Promise<void>;

let _open: OpenFn | null = null;

export function register_compose_dialog(open: OpenFn): void {
	_open = open;
}

export function open_compose(context?: ComposeOpenContext): void {
	_open?.(context);
}
