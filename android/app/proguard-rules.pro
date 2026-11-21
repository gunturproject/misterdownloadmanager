# Called from Rust through JNI; Proguard cannot see those references.
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }
