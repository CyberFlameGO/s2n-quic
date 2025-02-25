// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

macro_rules! aes_impl {
    ($name:ident) => {
        pub mod $name {
            use super::Aes;
            use lazy_static::lazy_static;

            pub use crate::aes::$name::KEY_LEN;

            pub struct Implementation {
                pub(crate) name: &'static str,
                pub(crate) new: fn(key: [u8; KEY_LEN]) -> Box<dyn Aes>,
            }

            impl Implementation {
                pub fn name(&self) -> &'static str {
                    self.name
                }

                #[allow(clippy::new_ret_no_self)]
                pub fn new(&self, key: [u8; KEY_LEN]) -> Box<dyn Aes> {
                    (self.new)(key)
                }
            }

            lazy_static! {
                static ref IMPLEMENTATIONS: Vec<Implementation> = {
                    let mut impls = vec![];

                    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
                    crate::aes::x86::testing::$name::implementations(&mut impls);

                    #[cfg(any(test, feature = "aes"))]
                    super::rust_crypto::$name::implementations(&mut impls);

                    impls
                };
            }

            pub fn implementations() -> &'static [Implementation] {
                &*IMPLEMENTATIONS
            }
        }
    };
}

aes_impl!(aes128);
aes_impl!(aes256);

pub use crate::aes::BLOCK_LEN;

pub trait Aes {
    fn encrypt(&self, input: &mut [u8]);
    fn decrypt(&self, input: &mut [u8]);
}

#[inline(always)]
pub fn for_each_block<F: FnMut(&mut [u8; BLOCK_LEN])>(input: &mut [u8], mut f: F) {
    use core::convert::TryInto;

    for chunk in input.chunks_exact_mut(BLOCK_LEN) {
        let block: &mut [u8; BLOCK_LEN] = chunk.try_into().unwrap();
        f(block)
    }
}

#[cfg(any(test, feature = "aes"))]
mod rust_crypto;
