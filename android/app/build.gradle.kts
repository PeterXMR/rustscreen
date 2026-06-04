plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.rustscreen.client"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.rustscreen.client"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    ndkVersion = "25.2.9519653"

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    kotlinOptions {
        jvmTarget = "1.8"
    }

    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    // Release signing scaffold (P8). No keystore or password is committed — the
    // config reads them from the environment (preferred for CI) or from a personal,
    // git-ignored `keystore.properties` at the android/ root. If neither is present,
    // a release build is signed with the DEBUG key and a clear warning is printed,
    // so `./gradlew assembleRelease` still works locally for smoke-testing without
    // secrets. NEVER commit a real keystore or its passwords. See packaging/android.
    val keystorePropsFile = rootProject.file("keystore.properties")
    val keystoreProps = java.util.Properties().apply {
        if (keystorePropsFile.exists()) {
            keystorePropsFile.inputStream().use { load(it) }
        }
    }
    fun signingValue(envName: String, propName: String): String? =
        System.getenv(envName) ?: keystoreProps.getProperty(propName)

    val storeFilePath = signingValue("RUSTSCREEN_KEYSTORE", "storeFile")
    val hasReleaseSigning = storeFilePath != null

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = file(storeFilePath!!)
                storePassword = signingValue("RUSTSCREEN_KEYSTORE_PASSWORD", "storePassword")
                keyAlias = signingValue("RUSTSCREEN_KEY_ALIAS", "keyAlias")
                keyPassword = signingValue("RUSTSCREEN_KEY_PASSWORD", "keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("release")
            } else {
                // Fall back to the debug key so an unsigned-for-distribution release
                // APK can still be built locally. NOT suitable for distribution.
                logger.warn(
                    "RustScreen: no release keystore configured " +
                        "(set RUSTSCREEN_KEYSTORE env or android/keystore.properties); " +
                        "signing assembleRelease with the DEBUG key — do NOT distribute this APK."
                )
                signingConfigs.getByName("debug")
            }
        }
    }
}

dependencies {
    // The thin shell (D7) uses only framework APIs (android.app.Activity, SurfaceView)
    // and loads the Rust cdylib via JNI — no AndroidX dependencies are required.
}
