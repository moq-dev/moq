//! DTLS 1.3 protocol implementation.
//!

// This is the full DTLS 1.3 handshake flow (RFC 9147)
//
// Client                                               Server
//
// ClientHello
//  + supported_versions
//  + supported_groups
//  + key_share
//  + signature_algorithms             -------->
//                                                       ServerHello
//                                                        + key_share
//                                               + supported_versions
//                                               {EncryptedExtensions}
//                                               {CertificateRequest*}
//                                                      {Certificate}
//                                                {CertificateVerify}
//                                     <--------          {Finished}
// {Certificate*}
// {CertificateVerify*}
// {Finished}                          -------->
// [Application Data]                  <------->   [Application Data]
//
// {} = encrypted with handshake keys (epoch 2)
// [] = encrypted with application keys (epoch 3)
// *  = optional (client auth)

pub mod incoming;
pub mod message;

mod client;
mod engine;
mod queue;
mod server;

pub use client::Client;
pub use server::Server;
