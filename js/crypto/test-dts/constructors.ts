import { Consumer, Producer } from "../dist/e2ee/generation-internal.js";
import { E2ee } from "../dist/index.js";

// @ts-expect-error A credential binds a consuming generation to an epoch.
new Consumer();
// @ts-expect-error A credential mints a producing generation and its epoch.
new Producer();
// @ts-expect-error The internal factory is not a package export, including type-only namespaces.
export type Factory = typeof E2ee.Generation.makeProducer;

const credential = new E2ee.Credential({ context: new Uint8Array(), kid: 1, secret: new Uint8Array(32) });
const producer: E2ee.Generation.Producer = credential.produce();
const consumer: E2ee.Generation.Consumer = credential.consume(producer.epoch);
void consumer.name("test");
// @ts-expect-error Construction stays private even with valid arguments.
new Consumer(credential, producer.epoch);
// @ts-expect-error Construction stays private even with valid arguments.
new Producer(credential);
