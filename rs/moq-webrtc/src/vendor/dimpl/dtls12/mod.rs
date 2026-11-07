//! DTLS 1.2 protocol implementation.
//!

// This is the full DTLS 1.2 handshake flow
//
// Client                                               Server
//
// 1     ClientHello                  -------->
//
// 2                                  <--------   HelloVerifyRequest
//                                                 (contains cookie)
//
// 3     ClientHello                  -------->
//       (with cookie)
// 4                                                     ServerHello
//                                                      Certificate*
//                                                ServerKeyExchange*
//                                               CertificateRequest*
//                                    <--------      ServerHelloDone
// 5     Certificate*
//       ClientKeyExchange
//       CertificateVerify*
//       [ChangeCipherSpec]
//       Finished                     -------->
// 6                                              [ChangeCipherSpec]
//                                    <--------             Finished
//       Application Data             <------->     Application Data

mod client;
mod context;
mod engine;
pub mod incoming;
pub mod message;
mod queue;
mod server;

pub use client::Client;
pub use server::Server;
