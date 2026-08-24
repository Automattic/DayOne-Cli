use std::ops::Deref;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.0.as_str()
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.0.as_str()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

id_type!(JournalId);
id_type!(EntryId);
id_type!(MomentId);
id_type!(ContentKeyFingerprint);
