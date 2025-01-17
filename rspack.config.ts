import {defineConfig} from "@rspack/cli";

const targets = ["chrome >= 87", "edge >= 88", "firefox >= 78", "safari >= 14"];

export default defineConfig({
    entry: {
        main: "./src/index.ts"
    },
    resolve: {
        extensions: ["...", ".ts"]
    },
    module: {
        rules: [
            {
                test: /\.svg$/,
                type: "asset"
            },
            {
                test: /\.js$/,
                use: [
                    {
                        loader: "builtin:swc-loader",
                        options: {
                            jsc: {
                                parser: {
                                    syntax: "ecmascript"
                                }
                            },
                            env: {targets}
                        }
                    }
                ]
            },
            {
                test: /\.ts$/,
                use: [
                    {
                        loader: "builtin:swc-loader",
                        options: {
                            jsc: {
                                parser: {
                                    syntax: "typescript"
                                }
                            },
                            env: {targets}
                        }
                    }
                ]
            },
            {
                test: /\.css$/,
                use: [
                    {
                        loader: 'postcss-loader',
                        options: {
                            postcssOptions: {
                                plugins: {
                                    tailwindcss: {},
                                    autoprefixer: {},
                                },
                            },
                        },
                    },
                ],
                type: "css",
            },
        ]
    },
    plugins: [],
    experiments: {
        css: true
    }
});
