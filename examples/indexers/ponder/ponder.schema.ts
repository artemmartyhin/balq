import { onchainTable } from "ponder";

/** WETH balance of every account a Transfer touched, as of that Transfer's block. */
export const holder = onchainTable("holder", (t) => ({
  address: t.hex().primaryKey(),
  balance: t.bigint().notNull(),
  block: t.bigint().notNull(),
}));
