// pageon's IndexedDB: fake-indexeddb 6.2.8 (Apache-2.0) with the session's persistence. The
// bundle is one function the engine calls with its host: `load()` returns the origin's database
// snapshots, `save(name, snapshot | null)` keeps one (false: over the origin's limit).
// Built from a directory where `npm i fake-indexeddb@6.2.8` ran (see indexeddb.rs).
import {
  indexedDB, IDBCursor, IDBCursorWithValue, IDBDatabase, IDBFactory, IDBIndex, IDBKeyRange,
  IDBObjectStore, IDBOpenDBRequest, IDBRecord, IDBRequest, IDBTransaction, IDBVersionChangeEvent,
} from './node_modules/fake-indexeddb/build/esm/index.js';
import Database from './node_modules/fake-indexeddb/build/esm/lib/Database.js';
import ObjectStore from './node_modules/fake-indexeddb/build/esm/lib/ObjectStore.js';
import Index from './node_modules/fake-indexeddb/build/esm/lib/Index.js';

globalThis.__pageonIndexedDB = (host) => {
  const databases = indexedDB._databases;
  const known = new Set();

  const exportDb = (db) => ({
    name: db.name,
    version: db.version,
    stores: [...db.rawObjectStores.values()].filter((s) => !s.deleted).map((s) => ({
      name: s.name,
      keyPath: s.keyPath,
      autoIncrement: s.autoIncrement,
      keyGen: s.keyGenerator ? s.keyGenerator.num : 0,
      records: [...s.records.records.getAllRecords()].map((r) => [r.key, r.value]),
      indexes: [...s.rawIndexes.values()].filter((i) => !i.deleted).map((i) => ({
        name: i.name, keyPath: i.keyPath, unique: i.unique, multiEntry: i.multiEntry,
      })),
    })),
  });

  const importDb = (snap) => {
    const db = new Database(snap.name, snap.version);
    for (const s of snap.stores) {
      const store = new ObjectStore(db, s.name, s.keyPath, s.autoIncrement);
      if (store.keyGenerator) store.keyGenerator.num = s.keyGen;
      for (const [key, value] of s.records) store.records.put({ key, value });
      for (const i of s.indexes) {
        const index = new Index(store, i.name, i.keyPath, i.multiEntry, i.unique);
        for (const record of store.records.records.getAllRecords()) index.storeRecord(record);
        index.initialized = true;
        store.rawIndexes.set(i.name, index);
      }
      db.rawObjectStores.set(s.name, store);
    }
    databases.set(snap.name, db);
    known.add(snap.name);
  };

  for (const snap of host.load()) {
    try {
      importDb(snap);
    } catch (e) {
      reportError(new Error(`IndexedDB: the saved database ${snap.name} could not be restored (${e})`));
    }
  }

  // Writing commits mark their database; one flush per task keeps the marked ones.
  const dirty = new Set();
  let scheduled = false;
  const flush = () => {
    scheduled = false;
    for (const name of dirty) {
      const db = databases.get(name);
      if (!db) {
        if (known.delete(name)) host.save(name, null);
        continue;
      }
      if (host.save(name, exportDb(db))) {
        known.add(name);
      } else {
        reportError(new DOMException(
          `IndexedDB: ${name} is over the origin's 50 MiB and was not kept`, 'QuotaExceededError'));
      }
    }
    dirty.clear();
  };
  const mark = (name) => {
    dirty.add(name);
    if (!scheduled) {
      scheduled = true;
      setTimeout(flush, 0);
    }
  };
  const dispatch = IDBTransaction.prototype.dispatchEvent;
  IDBTransaction.prototype.dispatchEvent = function dispatchEvent(event) {
    const r = dispatch.call(this, event);
    if (event.type === 'complete' && this.mode !== 'readonly') mark(this.db.name);
    return r;
  };
  const remove = indexedDB.deleteDatabase;
  indexedDB.deleteDatabase = function deleteDatabase(name, ...rest) {
    const request = remove.call(this, name, ...rest);
    request.addEventListener('success', () => mark(String(name)));
    return request;
  };

  return {
    indexedDB, IDBCursor, IDBCursorWithValue, IDBDatabase, IDBFactory, IDBIndex, IDBKeyRange,
    IDBObjectStore, IDBOpenDBRequest, IDBRecord, IDBRequest, IDBTransaction, IDBVersionChangeEvent,
  };
};
