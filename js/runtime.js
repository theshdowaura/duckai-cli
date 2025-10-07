(function (global) {
  const ABSENT = Symbol('absent');

  function base64Decode(str) {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=';
    let output = '';
    let buffer = 0;
    let bits = 0;
    for (let i = 0; i < str.length; i += 1) {
      const ch = str.charAt(i);
      if (ch === '=') {
        break;
      }
      const value = chars.indexOf(ch);
      if (value === -1) {
        continue;
      }
      buffer = (buffer << 6) | value;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        const code = (buffer >> bits) & 0xff;
        output += String.fromCharCode(code);
      }
    }
    return output;
  }

  function createBrowserEnv(userAgent) {
    const windowObj = global;
    const restorers = [];

    const storeDescriptor = Object.getOwnPropertyDescriptor(windowObj, Symbol.toStringTag);
    Object.defineProperty(windowObj, Symbol.toStringTag, { value: 'Window', configurable: true });
    restorers.push(() => {
      if (storeDescriptor) {
        Object.defineProperty(windowObj, Symbol.toStringTag, storeDescriptor);
      } else {
        delete windowObj[Symbol.toStringTag];
      }
    });

    const setProp = (key, value) => {
      const hadOwn = Object.prototype.hasOwnProperty.call(windowObj, key);
      const previousDescriptor = hadOwn ? Object.getOwnPropertyDescriptor(windowObj, key) : undefined;
      const defined = Reflect.defineProperty(windowObj, key, {
        configurable: true,
        enumerable: true,
        writable: true,
        value,
      });
      if (!defined) {
        windowObj[key] = value;
      }
      restorers.push(() => {
        if (previousDescriptor) {
          Object.defineProperty(windowObj, key, previousDescriptor);
        } else {
          delete windowObj[key];
        }
      });
    };

    class BaseElement {
      constructor(tagName) {
        this.tagName = tagName.toUpperCase();
        this._children = [];
        this.parentNode = null;
        this._attributes = Object.create(null);
        this._innerHTML = '';
        this._textContent = '';
        this.style = {};
        this.classList = {
          add() {},
          remove() {},
          contains() { return false; },
        };
      }
      appendChild(child) {
        child.parentNode = this;
        this._children.push(child);
        return child;
      }
      removeChild(child) {
        const idx = this._children.indexOf(child);
        if (idx !== -1) {
          this._children.splice(idx, 1);
          child.parentNode = null;
        }
        return child;
      }
      setAttribute(name, value) {
        const key = String(name).toLowerCase();
        this._attributes[key] = String(value);
        if (key === 'id') {
          this.id = value;
        }
      }
      getAttribute(name) {
        const key = String(name).toLowerCase();
        return this._attributes[key] ?? null;
      }
      set innerHTML(value) {
        this._innerHTML = String(value);
      }
      get innerHTML() {
        return this._innerHTML;
      }
      set textContent(value) {
        this._textContent = String(value);
      }
      get textContent() {
        return this._textContent;
      }
      querySelector(selector) {
        return this.ownerDocument ? this.ownerDocument.querySelector(selector) : null;
      }
      querySelectorAll(selector) {
        return this.ownerDocument ? this.ownerDocument.querySelectorAll(selector) : new NodeList();
      }
    }

    class HTMLCollection {
      constructor(source) {
        this._source = source;
      }
      get length() {
        return this._source.length;
      }
      item(index) {
        return this._source[index] || null;
      }
      [Symbol.iterator]() {
        return this._source[Symbol.iterator]();
      }
    }

    class NodeList {
      constructor(items = []) {
        this._items = Array.from(items);
        this._syncIndexAccess();
      }

      _syncIndexAccess() {
        Object.keys(this).forEach((key) => {
          if (/^\d+$/.test(key)) {
            delete this[key];
          }
        });
        this._items.forEach((value, index) => {
          Object.defineProperty(this, index, {
            configurable: true,
            enumerable: true,
            get: () => this._items[index],
            set: (val) => {
              this._items[index] = val;
            },
          });
        });
      }

      get length() {
        return this._items.length;
      }

      item(index) {
        return this._items[index] ?? null;
      }

      entries() {
        return this._items.entries();
      }

      forEach(callback, thisArg) {
        this._items.forEach(callback, thisArg);
      }

      [Symbol.iterator]() {
        return this._items[Symbol.iterator]();
      }
    }

    Object.defineProperty(NodeList.prototype, Symbol.toStringTag, {
      value: 'NodeList',
    });

    class Element extends BaseElement {}
    class HTMLElement extends Element {}
    class HTMLDivElement extends HTMLElement {}

    class Document {
      constructor() {
        this._body = new (class extends HTMLElement {
          constructor() {
            super('body');
          }
          get children() {
            return new HTMLCollection(this._children);
          }
        })('body');
        this._registry = new Map();
      }
      get body() {
        return this._body;
      }
      createElement(tag) {
        const lower = String(tag).toLowerCase();
        let el;
        if (lower === 'div') el = new HTMLDivElement('div');
        else if (lower === 'iframe') el = new HTMLIFrameElement();
        else el = new HTMLElement(tag);
        el.ownerDocument = this;
        return el;
      }
      register(selector, element) {
        this._registry.set(selector, element);
        return element;
      }
      querySelector(selector) {
        if (this._registry.has(selector)) {
          return this._registry.get(selector);
        }
        const stub = this._createStubForSelector(selector);
        this._registry.set(selector, stub);
        return stub;
      }
      querySelectorAll(selector) {
        if (this._registry.has(selector)) {
          const el = this._registry.get(selector);
          return el ? new NodeList([el]) : new NodeList();
        }
        const stub = this._createStubForSelector(selector);
        this._registry.set(selector, stub);
        return new NodeList([stub]);
      }
      _createStubForSelector(selector) {
        const tagMatch = selector && selector.match(/[a-z0-9_-]+$/i);
        const tag = tagMatch ? tagMatch[0].replace(/^#|\./g, '') : 'div';
        const el = new HTMLElement(tag || 'div');
        el.ownerDocument = this;
        el.dataset = Object.create(null);
        el.value = '';
        el.children = [];
        el.offsetHeight = 0;
        el.offsetWidth = 0;
        el.getBoundingClientRect = () => ({ width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 });
        el.cloneNode = () => this._createStubForSelector(selector);
        return el;
      }
    }

    class HTMLIFrameElement extends HTMLElement {
      constructor() {
        super('iframe');
        this.contentDocument = new Document();
        const frameWindow = Object.create(windowObj);
        frameWindow.document = this.contentDocument;
        frameWindow.top = windowObj;
        frameWindow.self = frameWindow;
        frameWindow.window = frameWindow;
        frameWindow.toString = () => '[object Window]';
        this.contentWindow = frameWindow;
        this.srcdoc = '';
      }
    }

    const document = new Document();
    const ua = String(userAgent || '');
    const isWin = /Windows NT/.test(ua);
    const isMac = /Macintosh;/.test(ua);
    const isIOS = /iPhone;/.test(ua);
    const isEdge = /Edg\//.test(ua);
    const isChrome = /Chrome\//.test(ua) && !isEdge;
    const platform = isWin ? 'Win32' : (isMac ? 'MacIntel' : (isIOS ? 'iPhone' : 'Linux x86_64'));
    const vendor = isChrome || isEdge ? 'Google Inc.' : (isMac || isIOS ? 'Apple Computer, Inc.' : '');

    const navLang = 'en-US';
    const navigator = {
      userAgent: ua,
      webdriver: false,
      platform,
      vendor,
      language: navLang,
      languages: [navLang].concat(navLang.startsWith('en') ? ['en'] : ['zh-CN','zh','en-US','en']).filter((v,i,a)=>a.indexOf(v)===i),
      hardwareConcurrency: 8,
      deviceMemory: 8,
      maxTouchPoints: isIOS ? 5 : 0,
    };
    try {
      const verMatch = ua.match(/(Chrome|Edg)\/(\d+)/);
      const ver = (verMatch && verMatch[2]) || '124';
      navigator.userAgentData = {
        brands: isEdge
          ? [{ brand: 'Chromium', version: ver }, { brand: 'Microsoft Edge', version: ver }, { brand: 'Not_A Brand', version: '99' }]
          : [{ brand: 'Chromium', version: ver }, { brand: 'Google Chrome', version: ver }, { brand: 'Not-A.Brand', version: '99' }],
        mobile: !!isIOS,
        platform: isWin ? 'Windows' : (isMac ? 'macOS' : (isIOS ? 'iOS' : 'Linux')),
        getHighEntropyValues: async () => ({ architecture: 'x86', bitness: '64', model: '', platformVersion: '15.0.0' })
      };
    } catch {}

    setProp('navigator', navigator);
    setProp('document', document);
    setProp('self', windowObj);
    setProp('window', windowObj);
    setProp('top', windowObj);
    setProp('screen', { width: 1920, height: 1080, availWidth: 1920, availHeight: 1040, colorDepth: 24, pixelDepth: 24 });
    setProp('devicePixelRatio', 1);
    setProp('chrome', { runtime: {} });
    setProp('location', { href: 'https://duckduckgo.com/duckchat', origin: 'https://duckduckgo.com', protocol: 'https:', host: 'duckduckgo.com', hostname: 'duckduckgo.com', pathname: '/duckchat' });
    document.referrer = 'https://duckduckgo.com/';

    const atobShim = (s) => base64Decode(String(s));
    const btoaShim = (s) => {
      const bytes = String(s).split('').map((ch) => ch.charCodeAt(0) & 0xff);
      const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
      let out = '';
      for (let i = 0; i < bytes.length; i += 3) {
        const buffer = ((bytes[i] || 0) << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
        const pad = bytes.length - i;
        out += chars[(buffer >> 18) & 0x3f];
        out += chars[(buffer >> 12) & 0x3f];
        out += pad > 1 ? chars[(buffer >> 6) & 0x3f] : '=';
        out += pad > 2 ? chars[buffer & 0x3f] : '=';
      }
      return out;
    };

    if (typeof windowObj.atob !== 'function') setProp('atob', atobShim);
    if (typeof windowObj.btoa !== 'function') setProp('btoa', btoaShim);
    setProp('HTMLElement', HTMLElement);
    setProp('HTMLDivElement', HTMLDivElement);
    setProp('HTMLIFrameElement', HTMLIFrameElement);
    setProp('Element', Element);
    setProp('NodeList', NodeList);
    setProp('HTMLCollection', HTMLCollection);
    setProp('Promise', Promise);
    setProp('Array', Array);
    setProp('JSON', JSON);
    setProp('Proxy', Proxy);
    setProp('Symbol', Symbol);
    setProp('Math', Math);
    setProp('Error', Error);
    setProp('__DDG_FE_CHAT_HASH__', 'stub-hash');
    setProp('__DDG_BE_VERSION__', 'stub-be');
    setProp('Window', function WindowStub() {});
    setProp('Object', Object);
    setProp('2uZEzrZ', {});
    setProp('map', Array.prototype.map.bind([]));
    setProp('contentDocument', {});

    ['Window', 'Object', 'self', 'Proxy', '2uZEzrZ', 'map'].forEach((name) => {
      const alias = `_${name}`;
      if (!Object.prototype.hasOwnProperty.call(windowObj, alias)) {
        setProp(alias, windowObj[name]);
      }
    });

    const iframeStub = document.createElement('iframe');
    iframeStub.setAttribute('id', 'jsa');
    iframeStub.setAttribute('sandbox', 'allow-scripts allow-same-origin');
    iframeStub.contentWindow.self.top = windowObj;
    iframeStub.contentWindow.document = iframeStub.contentDocument;
    document.body.appendChild(iframeStub);
    document.register('#jsa', iframeStub);

    const metaStub = iframeStub.contentDocument.createElement('meta');
    metaStub.setAttribute('http-equiv', 'Content-Security-Policy');
    metaStub.setAttribute('content', "default-src 'none'; script-src 'unsafe-inline';");
    iframeStub.contentDocument.register('meta[http-equiv="Content-Security-Policy"]', metaStub);

    return {
      cleanup() {
        while (restorers.length) {
          const restore = restorers.pop();
          try {
            restore();
          } catch {
            // ignore cleanup errors
          }
        }
      },
    };
  }

  async function duckaiEvaluate(scriptB64, userAgent) {
    const env = createBrowserEnv(userAgent);
    try {
      const script = base64Decode(scriptB64);
      const result = global.eval(script);
      if (result && typeof result.then === 'function') {
        return await result;
      }
      return result;
    } finally {
      env.cleanup();
    }
  }

  global.duckaiEvaluate = duckaiEvaluate;
})(typeof globalThis !== 'undefined' ? globalThis : this);
