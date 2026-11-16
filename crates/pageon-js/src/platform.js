// Platform APIs that are script on the virtual clock: Web Animations (timing, events and
// promises; styles are not interpolated between keyframes), the Prioritized Task Scheduling
// API, and navigator.sendBeacon (a POST through the page's own fetch, so the egress policy,
// guard and recording see it like any write). Evaluated on first use: the prelude installs
// stubs that call it with the page's original fetch.
((fetch) => {
  const define = (o, name, value) =>
    Object.defineProperty(o, name, { value, writable: true, configurable: true, enumerable: false });
  const hidden = (o, values) => {
    for (const [k, v] of Object.entries(values)) define(o, k, v);
  };

  // A listener list for objects that are not native event targets.
  const listening = {
    addEventListener(type, cb, opts) {
      if (!cb) return;
      const once = typeof opts === 'object' && opts !== null && !!opts.once;
      const list = this.__listeners.get(type) || [];
      if (!list.some((l) => l.cb === cb)) list.push({ cb, once });
      this.__listeners.set(type, list);
    },
    removeEventListener(type, cb) {
      const list = this.__listeners.get(type);
      if (list) this.__listeners.set(type, list.filter((l) => l.cb !== cb));
    },
    dispatchEvent(event) {
      Object.defineProperty(event, 'target', { value: this, configurable: true });
      Object.defineProperty(event, 'currentTarget', { value: this, configurable: true });
      const handler = this['on' + event.type];
      const list = [...(this.__listeners.get(event.type) || [])];
      if (typeof handler === 'function') list.unshift({ cb: handler, once: false });
      for (const l of list) {
        if (l.once) this.removeEventListener(event.type, l.cb);
        try {
          if (typeof l.cb === 'function') l.cb.call(this, event);
          else if (l.cb && typeof l.cb.handleEvent === 'function') l.cb.handleEvent(event);
        } catch (e) {
          queueMicrotask(() => { throw e; });
        }
      }
      return !event.defaultPrevented;
    },
  };

  const later = (f) => setTimeout(f, 0);

  // ----- Web Animations -----
  if (globalThis.Element && globalThis.Document && globalThis.performance) {
    const timing = (t) => {
      if (typeof t === 'number') t = { duration: t };
      t = t || {};
      const r = {
        delay: Number(t.delay) || 0,
        endDelay: Number(t.endDelay) || 0,
        fill: t.fill || 'auto',
        iterationStart: Number(t.iterationStart) || 0,
        iterations: t.iterations === undefined ? 1 : Number(t.iterations),
        duration: t.duration === undefined || t.duration === 'auto' ? 'auto' : Number(t.duration),
        direction: t.direction || 'normal',
        easing: t.easing || 'linear',
      };
      if (!(r.iterations >= 0)) throw new TypeError('iterationCount must be non-negative.');
      if (r.duration !== 'auto' && !(r.duration >= 0)) throw new TypeError('duration must be non-negative or auto.');
      return r;
    };
    const active = (t) => (t.duration === 'auto' ? 0 : t.duration) * t.iterations || 0;
    const endOf = (t) => Math.max(t.delay + active(t) + t.endDelay, 0);
    const frames = (k) => {
      if (k == null) return [];
      if (Array.isArray(k)) return k.map((f) => ({ offset: null, easing: 'linear', composite: 'auto', ...f }));
      if (typeof k !== 'object') throw new TypeError('Keyframes must be an object or an array.');
      const props = Object.keys(k).filter((p) => !['offset', 'easing', 'composite'].includes(p));
      const n = Math.max(1, ...props.map((p) => (Array.isArray(k[p]) ? k[p].length : 1)));
      return Array.from({ length: n }, (_, i) => {
        const f = { offset: null, easing: 'linear', composite: 'auto' };
        for (const p of props) {
          const v = Array.isArray(k[p]) ? k[p][i] : i === n - 1 ? k[p] : undefined;
          if (v !== undefined) f[p] = String(v);
        }
        return f;
      });
    };

    class AnimationTimeline {
      get currentTime() { return performance.now(); }
    }
    class DocumentTimeline extends AnimationTimeline {
      constructor(options = {}) { super(); define(this, '__origin', Number(options.originTime) || 0); }
      get currentTime() { return performance.now() - this.__origin; }
    }
    const timeline = new DocumentTimeline();
    const live = new Set();

    class AnimationEffect {
      constructor(t) { define(this, '__timing', timing(t)); define(this, '__animation', null); }
      getTiming() { return { ...this.__timing }; }
      updateTiming(t = {}) {
        define(this, '__timing', timing({ ...this.__timing, ...t }));
        if (this.__animation) this.__animation.__update();
      }
      getComputedTiming() {
        const t = this.__timing;
        const a = this.__animation;
        const local = a ? a.currentTime : null;
        const d = t.duration === 'auto' ? 0 : t.duration;
        let progress = null;
        let iteration = null;
        if (local !== null && local >= t.delay && local <= t.delay + active(t) && d > 0) {
          const p = (local - t.delay) / d + t.iterationStart;
          iteration = Math.min(Math.floor(p), t.iterations - 1);
          progress = Math.min(p - iteration, 1);
        }
        return { ...t, duration: d, activeDuration: active(t), endTime: endOf(t), localTime: local, progress, currentIteration: iteration };
      }
    }
    class KeyframeEffect extends AnimationEffect {
      constructor(target, keyframes, options) {
        super(options);
        this.target = target || null;
        this.pseudoElement = (options && options.pseudoElement) || null;
        this.composite = (options && options.composite) || 'replace';
        define(this, '__frames', frames(keyframes));
      }
      getKeyframes() { return this.__frames.map((f) => ({ ...f })); }
      setKeyframes(k) { define(this, '__frames', frames(k)); }
    }

    class AnimationPlaybackEvent extends Event {
      constructor(type, init = {}) {
        super(type, init);
        Object.setPrototypeOf(this, new.target.prototype);
        define(this, '__current', init.currentTime ?? null);
        define(this, '__timeline', init.timelineTime ?? null);
      }
      get currentTime() { return this.__current; }
      get timelineTime() { return this.__timeline; }
    }

    class Animation {
      constructor(effect = null, tl = timeline) {
        hidden(this, {
          __listeners: new Map(), __effect: null, __timeline: tl, __rate: 1, __hold: null,
          __start: null, __state: 'idle', __timer: null,
        });
        this.id = '';
        this.onfinish = null;
        this.oncancel = null;
        this.onremove = null;
        this.replaceState = 'active';
        this.effect = effect;
        this.__promises();
      }
      __promises() {
        let resolve, reject;
        const p = new Promise((res, rej) => { resolve = res; reject = rej; });
        p.catch(() => {});
        hidden(this, { __finished: p, __resolve: resolve, __reject: reject });
      }
      get effect() { return this.__effect; }
      set effect(e) {
        define(this, '__effect', e || null);
        if (e) define(e, '__animation', this);
      }
      get timeline() { return this.__timeline; }
      get playState() { return this.__state; }
      get pending() { return false; }
      get ready() { return Promise.resolve(this); }
      get finished() { return this.__finished; }
      get playbackRate() { return this.__rate; }
      set playbackRate(r) { this.updatePlaybackRate(r); }
      get startTime() { return this.__state === 'running' ? this.__start : null; }
      get currentTime() {
        if (this.__state === 'idle') return null;
        if (this.__hold !== null) return this.__hold;
        return (this.__timeline.currentTime - this.__start) * this.__rate;
      }
      set currentTime(t) {
        if (t === null) throw new TypeError('currentTime cannot be null.');
        t = Number(t);
        if (this.__state === 'running') define(this, '__start', this.__timeline.currentTime - t / this.__rate);
        else define(this, '__hold', t);
        if (this.__state === 'idle' || this.__state === 'finished') {
          define(this, '__state', 'paused');
          define(this, '__hold', t);
        }
        this.__update();
      }
      __end() { return this.__effect ? endOf(this.__effect.__timing) : 0; }
      __clear() {
        if (this.__timer !== null) clearTimeout(this.__timer);
        define(this, '__timer', null);
      }
      __update() {
        this.__clear();
        if (this.__state !== 'running') return;
        const end = this.__end();
        const t = this.currentTime;
        if (this.__rate === 0 || (this.__rate > 0 && end === Infinity)) return;
        const left = this.__rate > 0 ? (end - t) / this.__rate : t / -this.__rate;
        define(this, '__timer', setTimeout(() => this.__finish(), Math.max(0, left)));
      }
      __finish() {
        this.__clear();
        const end = this.__end();
        define(this, '__hold', this.__rate >= 0 ? end : 0);
        define(this, '__state', 'finished');
        const fill = this.__effect ? this.__effect.__timing.fill : 'none';
        if (fill !== 'forwards' && fill !== 'both') live.delete(this);
        this.__resolve(this);
        later(() => {
          if (this.__state !== 'finished') return;
          this.dispatchEvent(new AnimationPlaybackEvent('finish', {
            currentTime: this.currentTime, timelineTime: this.__timeline.currentTime,
          }));
        });
      }
      play() {
        const end = this.__end();
        let t = this.currentTime;
        if (this.__rate > 0 && (t === null || t >= end || t < 0)) t = 0;
        if (this.__rate < 0 && (t === null || t <= 0 || t > end)) {
          if (end === Infinity) throw new DOMException('Cannot play reversed Animation with infinite target effect end.', 'InvalidStateError');
          t = end;
        }
        if (this.__state === 'finished') this.__promises();
        define(this, '__start', this.__timeline.currentTime - (this.__rate ? t / this.__rate : 0));
        define(this, '__hold', this.__rate ? null : t);
        define(this, '__state', 'running');
        live.add(this);
        this.__update();
      }
      pause() {
        if (this.__state === 'paused') return;
        const t = this.currentTime ?? (this.__rate < 0 ? this.__end() : 0);
        this.__clear();
        if (this.__state === 'finished') this.__promises();
        define(this, '__hold', t);
        define(this, '__state', 'paused');
        live.add(this);
      }
      finish() {
        const end = this.__end();
        if (this.__rate === 0 || (this.__rate > 0 && end === Infinity)) {
          throw new DOMException('Cannot finish Animation with a playbackRate of 0 or an infinite target effect end.', 'InvalidStateError');
        }
        this.__finish();
      }
      cancel() {
        if (this.__state === 'idle') return;
        this.__clear();
        define(this, '__state', 'idle');
        define(this, '__hold', null);
        define(this, '__start', null);
        live.delete(this);
        this.__reject(new DOMException('The user aborted a request.', 'AbortError'));
        this.__promises();
        later(() => this.dispatchEvent(new AnimationPlaybackEvent('cancel', {
          currentTime: null, timelineTime: this.__timeline.currentTime,
        })));
      }
      reverse() {
        define(this, '__rate', -this.__rate);
        if (this.__state === 'running' && this.currentTime !== null) {
          this.updatePlaybackRate(this.__rate);
        } else {
          this.play();
        }
      }
      updatePlaybackRate(r) {
        const t = this.currentTime;
        define(this, '__rate', Number(r));
        if (this.__state === 'running' && t !== null) {
          define(this, '__start', this.__timeline.currentTime - (this.__rate ? t / this.__rate : 0));
          define(this, '__hold', this.__rate ? null : t);
        }
        this.__update();
      }
      persist() { this.replaceState = 'persisted'; }
      commitStyles() {
        const target = this.__effect && this.__effect.target;
        const fs = this.__effect ? this.__effect.__frames : [];
        if (!target || !target.style || !fs.length) return;
        const end = this.__end();
        const t = this.currentTime;
        const f = t !== null && end > 0 && t / end < 0.5 ? fs[0] : fs[fs.length - 1];
        for (const [k, v] of Object.entries(f)) {
          if (['offset', 'easing', 'composite'].includes(k)) continue;
          target.style.setProperty(k.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase()), v);
        }
      }
    }
    Object.assign(Animation.prototype, listening);

    const relevant = (a) => a.__state !== 'idle' && live.has(a);
    hidden(Element.prototype, {
      animate(keyframes, options) {
        const a = new Animation(new KeyframeEffect(this, keyframes, options), timeline);
        if (options && typeof options === 'object' && options.id) a.id = String(options.id);
        a.play();
        return a;
      },
      getAnimations(options) {
        const deep = !!(options && options.subtree);
        return [...live].filter((a) => {
          const t = a.__effect && a.__effect.target;
          return relevant(a) && t && (t === this || (deep && this.contains(t)));
        });
      },
    });
    hidden(Document.prototype, {
      getAnimations() { return [...live].filter(relevant); },
    });
    Object.defineProperty(Document.prototype, 'timeline', {
      get() { return timeline; }, configurable: true, enumerable: true,
    });
    for (const C of [Animation, AnimationEffect, KeyframeEffect, AnimationTimeline, DocumentTimeline, AnimationPlaybackEvent]) {
      define(globalThis, C.name, C);
    }
  }

  // ----- Prioritized Task Scheduling -----
  if (globalThis.setTimeout && globalThis.AbortController) {
    const RANK = { 'user-blocking': 0, 'user-visible': 2, background: 4 };
    // Continuations of `yield` run before tasks of their priority.
    const queues = [[], [], [], [], [], []];
    let posted = false;
    const run = () => {
      posted = false;
      const q = queues.find((x) => x.length);
      if (q) q.shift()();
      if (queues.some((x) => x.length)) post();
    };
    let post;
    if (globalThis.MessageChannel) {
      const mc = new MessageChannel();
      mc.port1.onmessage = run;
      post = () => { if (!posted) { posted = true; mc.port2.postMessage(0); } };
    } else {
      post = () => { if (!posted) { posted = true; setTimeout(run, 0); } };
    }
    const check = (p) => {
      if (!(p in RANK)) throw new TypeError(`The provided value '${p}' is not a valid enum value of type TaskPriority.`);
      return p;
    };

    const TaskSignal = function TaskSignal() { throw new TypeError('Illegal constructor'); };
    TaskSignal.prototype = Object.create(AbortSignal.prototype, {
      constructor: { value: TaskSignal, writable: true, configurable: true },
      priority: { get() { return this.__priority; }, configurable: true, enumerable: true },
      onprioritychange: {
        get() { return this.__onchange || null; },
        set(f) {
          if (this.__onchange) this.removeEventListener('prioritychange', this.__onchange);
          define(this, '__onchange', typeof f === 'function' ? f : null);
          if (this.__onchange) this.addEventListener('prioritychange', this.__onchange);
        },
        configurable: true, enumerable: true,
      },
    });
    Object.setPrototypeOf(TaskSignal, AbortSignal);

    class TaskPriorityChangeEvent extends Event {
      constructor(type, init = {}) {
        super(type, init);
        Object.setPrototypeOf(this, new.target.prototype);
        define(this, '__previous', check(init.previousPriority));
      }
      get previousPriority() { return this.__previous; }
    }

    class TaskController extends AbortController {
      constructor(init = {}) {
        super();
        Object.setPrototypeOf(this, new.target.prototype);
        const s = this.signal;
        Object.setPrototypeOf(s, TaskSignal.prototype);
        define(s, '__priority', check((init && init.priority) || 'user-visible'));
        define(this, '__changing', false);
      }
      setPriority(p) {
        check(p);
        if (this.__changing) throw new DOMException('Priority change in progress.', 'NotAllowedError');
        const s = this.signal;
        if (s.__priority === p) return;
        const previous = s.__priority;
        define(s, '__priority', p);
        define(this, '__changing', true);
        try {
          s.dispatchEvent(new TaskPriorityChangeEvent('prioritychange', { previousPriority: previous }));
        } finally {
          define(this, '__changing', false);
        }
      }
    }

    class Scheduler {
      postTask(callback, options = {}) {
        if (typeof callback !== 'function') {
          return Promise.reject(new TypeError("Failed to execute 'postTask' on 'Scheduler': parameter 1 is not of type 'Function'."));
        }
        const signal = options.signal || null;
        let fixed;
        try {
          fixed = options.priority !== undefined ? check(options.priority) : null;
        } catch (e) {
          return Promise.reject(e);
        }
        if (signal && signal.aborted) return Promise.reject(signal.reason);
        return new Promise((resolve, reject) => {
          let done = false;
          let q = null;
          const onAbort = () => {
            if (done) return;
            done = true;
            if (q) q.splice(q.indexOf(task), 1);
            reject(signal.reason);
          };
          const task = () => {
            if (done) return;
            done = true;
            if (signal) signal.removeEventListener('abort', onAbort);
            try { resolve(callback()); } catch (e) { reject(e); }
          };
          if (signal) signal.addEventListener('abort', onAbort);
          const enqueue = () => {
            if (done) return;
            const p = fixed || (signal && signal.__priority) || 'user-visible';
            q = queues[RANK[p] + 1];
            q.push(task);
            post();
          };
          const delay = Math.max(0, Number(options.delay) || 0);
          if (delay > 0) setTimeout(enqueue, delay); else enqueue();
        });
      }
      yield() {
        return new Promise((resolve) => {
          queues[RANK['user-visible']].push(resolve);
          post();
        });
      }
    }
    const scheduler = new Scheduler();
    Object.defineProperty(globalThis, 'scheduler', { value: scheduler, writable: true, configurable: true, enumerable: true });
    for (const C of [Scheduler, TaskController, TaskSignal, TaskPriorityChangeEvent]) define(globalThis, C.name, C);
  }

  // ----- Beacons -----
  if (globalThis.Navigator && fetch) {
    const size = (d) => {
      if (d == null) return 0;
      if (typeof d === 'string') return new TextEncoder().encode(d).length;
      if (d instanceof Blob) return d.size;
      if (d instanceof ArrayBuffer || ArrayBuffer.isView(d)) return d.byteLength;
      if (d instanceof URLSearchParams) return new TextEncoder().encode(String(d)).length;
      return 0;
    };
    define(Navigator.prototype, 'sendBeacon', function sendBeacon(url, data = null) {
      if (arguments.length < 1) throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': 1 argument required, but only 0 present.");
      const u = new URL(String(url), document.baseURI);
      if (u.protocol !== 'http:' && u.protocol !== 'https:') {
        throw new TypeError("Failed to execute 'sendBeacon' on 'Navigator': Beacons are only supported over HTTP(S).");
      }
      if (size(data) > 65536) return false;
      fetch(u.href, { method: 'POST', body: data, keepalive: true, credentials: 'include' }).catch(() => {});
      return true;
    });
    Object.defineProperty(Navigator.prototype, 'sendBeacon', { enumerable: true });
  }
})
