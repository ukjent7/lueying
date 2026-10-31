/**
 * 掠影运行期脚本扩展的 TypeScript 声明(API 2)。
 *
 * 把本文件放进扩展源码目录,tsconfig 包含后即可获得 `lueying` 全局对象、
 * 场景与宿主服务调用的类型。这里只声明脚本可见的接口;清单字段见
 * `script.md`。
 */

declare namespace Lueying {
  /** `lueying.register` 收到的候选窗场景。脚本看不到候选文字与宿主名。 */
  interface Scene {
    api: number;
    composing: boolean;
    page: number;
    pages: number;
    candidates: number;
    selected: number;
    /** 0xRRGGBB。 */
    accent: number;
    dark: boolean;
    high_contrast: boolean;
    reduce_motion: boolean;
    time: number;
    anim: number;
    private: boolean;
  }

  interface EffectsPatch {
    /** 三个槽位的 SkSL 源码,键名由宿主定义。 */
    [slot: string]: string;
  }

  /** `register` 的返回值;只回主题色与装饰层。 */
  interface ScenePatch {
    accent?: number;
    effects?: EffectsPatch;
  }

  interface ShotOptions {
    region?: { x: number; y: number; width: number; height: number };
  }

  interface ShotResult {
    file: string;
    width: number;
    height: number;
  }

  interface RecognizeOptions {
    file: string;
    lang?: string;
    engine?: "system" | "ppocr";
  }

  interface RecognizeResult {
    text: string;
    lang: string;
  }

  interface DeliverResult {
    delivered: boolean;
  }

  interface PanelOptions {
    title: string;
    items: string[];
  }

  interface PanelResult {
    selected: number | null;
  }

  interface NetFetchOptions {
    url: string;
    method?: string;
    headers?: Record<string, string>;
    body?: string;
  }

  interface NetFetchResult {
    status: number;
    body: string;
  }

  interface HotkeyRegisterOptions {
    id: string;
    modifiers: string[];
    key: string;
  }

  interface ClipboardReadResult {
    text: string;
  }

  interface StoreGetResult {
    value: string | null;
  }

  interface StoreListResult {
    keys: string[];
  }

  interface Api {
    /** 场景协议版本,与 `Scene.api` 对应。 */
    readonly api: number;
    /** 注册候选窗场景处理函数;每个扩展只能调用一次。 */
    register(handler: (scene: Scene) => ScenePatch | void): void;
    /** 注册命令;完整 id 为 `<扩展 id>.<name>`。 */
    command(name: string, handler: () => unknown | Promise<unknown>): void;
    /** 宿主时钟上的延时;等待不占脚本预算。 */
    timeout(ms: number): Promise<void>;

    capture: {
      shot(options?: ShotOptions): Promise<ShotResult>;
    };
    ocr: {
      recognize(options: RecognizeOptions): Promise<RecognizeResult>;
    };
    clipboard: {
      read(): Promise<ClipboardReadResult>;
      write(options: { text: string }): Promise<Record<string, never>>;
    };
    store: {
      get(options: { key: string }): Promise<StoreGetResult>;
      set(options: { key: string; value: string }): Promise<Record<string, never>>;
      remove(options: { key: string }): Promise<Record<string, never>>;
      list(): Promise<StoreListResult>;
    };
    output: {
      deliver(options: { text: string }): Promise<DeliverResult>;
    };
    panel: {
      show(options: PanelOptions): Promise<PanelResult>;
    };
    net: {
      fetch(options: NetFetchOptions): Promise<NetFetchResult>;
    };
    hotkey: {
      register(options: HotkeyRegisterOptions): Promise<unknown>;
      unregister(options: { id: string }): Promise<unknown>;
      press(options: { id: string }): Promise<unknown>;
    };
    notify: {
      show(options: { title: string; body: string }): Promise<unknown>;
    };
  }
}

declare const lueying: Lueying.Api;

interface Window {
  lueying: Lueying.Api;
}
