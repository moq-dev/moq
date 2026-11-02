import { Consumer, Producer } from "../dist/generation-internal.js";
import { Credential, type Generation } from "../dist/index.js";

// @ts-expect-error A credential binds a consuming generation to an epoch.
new Consumer();
// @ts-expect-error A credential mints a producing generation and its epoch.
new Producer();
// @ts-expect-error The internal factory is not a package export, including type-only namespaces.
export type Factory = typeof Generation.makeProducer;

const credential = new Credential({ context: new Uint8Array(), kid: 1, secret: new Uint8Array(32) });
const producer: Generation.Producer = credential.produce();
const consumer: Generation.Consumer = credential.consume(producer.epoch);
void consumer.name("test");
// @ts-expect-error Construction stays private even with valid arguments.
new Consumer(credential, producer.epoch);
// @ts-expect-error Construction stays private even with valid arguments.
new Producer(credential);
