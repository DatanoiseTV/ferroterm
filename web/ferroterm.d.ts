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
  /** Right-click behavior: context menu, paste, or ignore (default 'menu'). */
  rightClick?: 'menu' | 'paste' | 'none';
  /** Extra right-click menu items appended after the built-in ones. */
  menuItems?: (ctx: { hasSelection: boolean }) => MenuItem[];
  /** Override link-click behavior instead of `window.open`. */
  onLink?: (uri: string, event: MouseEvent) => void;
  /** Override the WASM module URL (defaults to the packaged location). */
  wasmUrl?: string | URL;
}

export type Unsubscribe = () => void;

export interface MenuItem {
  label?: string;
  accel?: string;
  enabled?: boolean;
  separator?: boolean;
  action?: () => void;
}

export interface Match {
  /** Absolute logical line index (scrollback + screen). */
  line: number;
  col: number;
}

export const DEFAULT_THEME: Required<Theme>;

/** Initialize the WASM module (called automatically by `Ferroterm.create`). */
export function initWasm(wasmUrl?: string | URL): Promise<unknown>;

export class Ferroterm {
  /** Preload WASM. Call before `new Ferroterm(...)`. */
  static ready(wasmUrl?: string | URL): Promise<unknown>;
  /** Init WASM, construct the engine, and attach a view to `container`. */
  static create(container: HTMLElement, options?: FerrotermOptions): Promise<Ferroterm>;
  /** Construct the engine only (no view). Requires WASM to be ready. */
  constructor(options?: FerrotermOptions);

  /** Feed bytes or a string from the host / PTY into the terminal. */
  write(data: Uint8Array | string): void;

  onData(cb: (bytes: Uint8Array) => void): Unsubscribe;
  onTitleChange(cb: (title: string) => void): Unsubscribe;
  onBell(cb: () => void): Unsubscribe;
  onResize(cb: (cols: number, rows: number) => void): Unsubscribe;

  resize(cols: number, rows: number): void;
  fit(): void;
  focus(): void;
  blur(): void;

  /** Attach a renderer + input capture inside `container` and start drawing. */
  attachView(container: HTMLElement): void;
  /** Free the renderer/WebGL context + input, keeping all engine state. */
  detachView(): void;
  readonly attached: boolean;

  setRenderer(kind: 'webgl' | 'canvas'): void;
  readonly rendererName: string | null;
  setTheme(theme: Theme): void;
  setFontSize(px: number): void;
  clear(): void;

  getSelection(): string;
  selectAll(): void;
  clearSelection(): void;

  /** Search scrollback + screen (case-insensitive). */
  findAll(query: string): Match[];
  totalLines(): number;
  lineText(abs: number): string;
  scrollToLine(abs: number): void;
  scrollToBottom(): void;

  dispose(): void;

  readonly title: string;
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
