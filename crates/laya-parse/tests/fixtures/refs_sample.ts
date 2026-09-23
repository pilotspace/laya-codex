import { readFile } from "fs";
import Store, { Entry as E, openStore } from "./store";

export class Cache extends BaseCache implements Lookup {
  private store: Store;

  lookup(key: string): Entry | undefined {
    const raw = this.store.fetchRaw(key);
    console.log(raw);
    return decode<Entry>(raw);
  }
}

export function build(): Cache {
  return new Cache(createStore());
}
