export * from "./index";
import { Archive as NativeArchive, Layout } from "./index";

/** Thrown by reads when there is no value — never `null`. */
export class NotAvailableError extends Error {
  name: "NotAvailableError";
  code:
    | "NotWatched"
    | "BeforeStart"
    | "AfterHead"
    | "NotSynced"
    | "InvalidRange"
    | "NotBootstrapped"
    | "BootstrapPending"
    | "BootstrapLost"
    | "Internal";
}

/**
 * Storage of one contract at one block, read by variable name.
 * Generate a precise type with `balq typegen <layout> --name MyContractView`
 * (or `layout.typescript("MyContractView")`) and pass it as `T`.
 */
export type StorageView<T = any> = T;

export interface ContractView {
  /** Variables of the contract at the end of `block`. */
  at<T = any>(block: number): StorageView<T>;
}

declare module "./index" {
  interface Archive {
    /** Read storage by name through a solc layout; see `at()`. */
    view(address: string, layout: Layout): ContractView;
  }
}
