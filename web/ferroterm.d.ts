// Type definitions for the ferroterm web component.

export interface Theme {
  foreground?: string;
  background?: string;
  cursor?: string;
  cursorAccent?: string;
  selection?: string;
  /** 16 ANSI colors: 0-7 normal, 8-15 bright. */
  ansi?: string[];
}

export interface FerrotermOptions {
  cols?: number;
  rows?: number;
  scrollback?: number;
  fontFamily?: string;
  fontSize?: number;
  lineHeight?: number;
  renderer?: 'webgl' | 'canvas';
  theme?: Theme;
  cursorStyle?: 'block' | 'bar' | 'underline';
  cursorBlink?: boolean;
  scrollSensitivity?: number;
  /** Auto-fit to the container on resize (default true). */
  autoFit?: boolean;
  /** Copy to clipboard as soon as a selection is made. */
  copyOnSelect?: boolean;
  /** Override link-click behavior instead of `window.open`. */
  onLink?: (uri: string, event: MouseEvent) => void;
  /** Override the WASM module URL (defaults to the packaged location). */
  wasmUrl?: string | URL;
}

export type Unsubscribe = () => void;

export const DEFAULT_THEME: Required<Theme>;

/** Initialize the WASM module (called automatically by `Ferroterm.create`). */
export function initWasm(wasmUrl?: string | URL): Promise<unknown>;

export class Ferroterm {
  static create(container: HTMLElement, options?: FerrotermOptions): Promise<Ferroterm>;
  constructor(container: HTMLElement, options?: FerrotermOptions);

  /** Feed bytes or a string from the host / PTY into the terminal. */
  write(data: Uint8Array | string): void;

  /** Subscribe to user-generated output that should be sent to the PTY. */
  onData(cb: (bytes: Uint8Array) => void): Unsubscribe;
  onTitleChange(cb: (title: string) => void): Unsubscribe;
  onBell(cb: () => void): Unsubscribe;
  onResize(cb: (cols: number, rows: number) => void): Unsubscribe;

  resize(cols: number, rows: number): void;
  /** Resize to fill the container. */
  fit(): void;
  focus(): void;
  blur(): void;

  /** Switch renderer backend at runtime. */
  setRenderer(kind: 'webgl' | 'canvas'): void;
  readonly rendererName: string | null;

  setTheme(theme: Theme): void;

  getSelection(): string;
  clearSelection(): void;

  dispose(): void;

  readonly cols: number;
  readonly rows: number;
}

export default Ferroterm;

export class GridModel {
  cols: number;
  rows: number;
  cp: Uint32Array;
  fg: Uint32Array;
  bg: Uint32Array;
  flags: Uint16Array;
  link: Uint32Array;
  cursorX: number;
  cursorY: number;
  cursorVisible: boolean;
  rowText(y: number): string;
}

export class Palette {
  constructor(theme?: Theme, brightenBold?: boolean);
  setTheme(theme: Theme): void;
}

export class CanvasRenderer {}
export class WebGLRenderer {}
export const KEY: Record<string, number>;
