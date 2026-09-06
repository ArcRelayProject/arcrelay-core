use super::super::*;

#[test]
fn test_yaml_not_detected_as_code() {
    // Cover YAML structures that could be misclassified as code.
    let yaml_cases = [
        // Colons and indentation can resemble code.
        r#"openapi: 3.0.4
info:
  title: Admin User Management API
  description: 管理用户相关的 API 接口文档
  version: 1.0.0
  contact:
    name: API Support
    email: support@example.com

servers:
  - url: http://localhost:8080/api/v1
    description: 开发环境
  - url: https://api.example.com/api/v1
    description: 生产环境

security:
  - BearerAuth: []

paths:
  /admin/users:
    get:
      tags:
        - 用户管理
      summary: 获取用户列表
      description: 分页获取用户列表，支持多条件筛选
      operationId: getUsersList
      parameters:
        - name: page
          in: query
          description: 页码，默认1
          required: false
          schema:
            type: integer
            minimum: 1
            default: 1
        - name: size
          in: query
          description: 每页大小，默认20
          required: false
          schema:
            type: integer
            minimum: 1
            maximum: 100
            default: 20
        - name: username
          in: query
          description: 用户名模糊查询
          required: false
          schema:
            type: string
        - name: email
          in: query
          description: 邮箱模糊查询
          required: false
          schema:
            type: string
        - name: status
          in: query
          description: 用户状态筛选
          required: false
          schema:
            type: integer
            enum: [0, 1, 2]
            description: 0-禁用, 1-正常, 2-待激活
        - name: start_date
          in: query
          description: 注册开始时间 (ISO 8601格式)
          required: false
          schema:
            type: string
            format: date-time
        - name: end_date
          in: query
          description: 注册结束时间 (ISO 8601格式)
          required: false
          schema:
            type: string
            format: date-time
      responses:
        '200':
          description: 成功获取用户列表
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/UserListResponse'
        '400':
          description: 请求参数错误
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

    post:
      tags:
        - 用户管理
      summary: 创建用户
      description: 创建新用户账号
      operationId: createUser
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateUserRequest'
      responses:
        '201':
          description: 用户创建成功
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/UserInfo'
        '400':
          description: 请求参数错误
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '409':
          description: 用户名或邮箱已存在
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

  /admin/users/{user_id}:
    get:
      tags:
        - 用户管理
      summary: 获取用户详情
      description: 根据用户ID获取用户详细信息
      operationId: getUserById
      parameters:
        - name: user_id
          in: path
          description: 用户ID
          required: true
          schema:
            type: integer
            format: int64
      responses:
        '200':
          description: 成功获取用户详情
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/UserInfo'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '404':
          description: 用户不存在
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

    put:
      tags:
        - 用户管理
      summary: 更新用户信息
      description: 更新指定用户的信息
      operationId: updateUser
      parameters:
        - name: user_id
          in: path
          description: 用户ID
          required: true
          schema:
            type: integer
            format: int64
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/UpdateUserRequest'
      responses:
        '200':
          description: 用户信息更新成功
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/UserInfo'
        '400':
          description: 请求参数错误
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '404':
          description: 用户不存在
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

    delete:
      tags:
        - 用户管理
      summary: 删除用户
      description: 删除指定用户
      operationId: deleteUser
      parameters:
        - name: user_id
          in: path
          description: 用户ID
          required: true
          schema:
            type: integer
            format: int64
      responses:
        '204':
          description: 用户删除成功
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '404':
          description: 用户不存在
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

  /admin/users/{user_id}/reset-password:
    post:
      tags:
        - 用户管理
      summary: 重置用户密码
      description: 管理员重置指定用户的密码
      operationId: resetUserPassword
      parameters:
        - name: user_id
          in: path
          description: 用户ID
          required: true
          schema:
            type: integer
            format: int64
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/ResetPasswordRequest'
      responses:
        '204':
          description: 密码重置成功
        '400':
          description: 请求参数错误
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '404':
          description: 用户不存在
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

  /admin/users/statistics:
    get:
      tags:
        - 用户管理
      summary: 获取用户统计信息
      description: 获取用户相关的统计数据
      operationId: getUserStatistics
      responses:
        '200':
          description: 成功获取统计信息
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/UserStatisticsResponse'
        '401':
          description: 未授权
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'
        '403':
          description: 权限不足
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ErrorResponse'

components:
  securitySchemes:
    BearerAuth:
      type: http
      scheme: bearer
      bearerFormat: JWT
      description: JWT Token 认证

  schemas:
    UserListResponse:
      type: object
      properties:
        users:
          type: array
          items:
            $ref: '#/components/schemas/UserInfo'
          description: 用户列表
        total:
          type: integer
          format: int64
          description: 总数
        page:
          type: integer
          format: int64
          description: 当前页
        size:
          type: integer
          format: int64
          description: 每页大小
      required:
        - users
        - total
        - page
        - size

    UserInfo:
      type: object
      properties:
        id:
          type: integer
          format: int64
          description: 用户ID
        username:
          type: string
          description: 用户名
        nickname:
          type: string
          description: 昵称
        email:
          type: string
          format: email
          nullable: true
          description: 邮箱
        phone:
          type: string
          nullable: true
          description: 手机号
        avatar:
          type: string
          nullable: true
          description: 头像URL
        status:
          type: integer
          description: 用户状态 (0-禁用, 1-正常, 2-待激活)
          enum: [0, 1, 2]
        is_active:
          type: boolean
          nullable: true
          description: 是否激活
        is_superuser:
          type: boolean
          nullable: true
          description: 是否超级用户
        created_time:
          type: string
          format: date-time
          nullable: true
          description: 创建时间
        inviter_id:
          type: integer
          format: int64
          nullable: true
          description: 邀请人ID
      required:
        - id
        - username
        - nickname
        - status

    CreateUserRequest:
      type: object
      properties:
        username:
          type: string
          description: 用户名
          minLength: 3
          maxLength: 50
        nickname:
          type: string
          description: 昵称
          minLength: 1
          maxLength: 100
        email:
          type: string
          format: email
          description: 邮箱
        phone:
          type: string
          nullable: true
          description: 手机号
          pattern: '^1[3-9]\d{9}$'
        password:
          type: string
          description: 密码
          minLength: 6
          maxLength: 128
        status:
          type: integer
          nullable: true
          description: 用户状态 (0-禁用, 1-正常, 2-待激活)
          enum: [0, 1, 2]
          default: 1
        is_active:
          type: boolean
          nullable: true
          description: 是否激活
          default: true
      required:
        - username
        - nickname
        - email
        - password

    UpdateUserRequest:
      type: object
      properties:
        nickname:
          type: string
          nullable: true
          description: 昵称
          minLength: 1
          maxLength: 100
        email:
          type: string
          format: email
          nullable: true
          description: 邮箱
        phone:
          type: string
          nullable: true
          description: 手机号
          pattern: '^1[3-9]\d{9}$'
        status:
          type: integer
          nullable: true
          description: 用户状态 (0-禁用, 1-正常, 2-待激活)
          enum: [0, 1, 2]
        is_active:
          type: boolean
          nullable: true
          description: 是否激活

    ResetPasswordRequest:
      type: object
      properties:
        new_password:
          type: string
          description: 新密码
          minLength: 6
          maxLength: 128
      required:
        - new_password

    UserStatisticsResponse:
      type: object
      properties:
        total_users:
          type: integer
          format: int64
          description: 总用户数
        active_users:
          type: integer
          format: int64
          description: 活跃用户数
        new_users_today:
          type: integer
          format: int64
          description: 今日新增用户
        new_users_this_week:
          type: integer
          format: int64
          description: 本周新增用户
        membership_stats:
          $ref: '#/components/schemas/MembershipStats'
      required:
        - total_users
        - active_users
        - new_users_today
        - new_users_this_week
        - membership_stats

    MembershipStats:
      type: object
      properties:
        free:
          type: integer
          format: int64
          description: 免费用户数
        premium:
          type: integer
          format: int64
          description: 付费用户数
      required:
        - free
        - premium

    ErrorResponse:
      type: object
      properties:
        error:
          type: string
          description: 错误信息
        code:
          type: string
          description: 错误代码
        message:
          type: string
          description: 详细错误描述
        timestamp:
          type: string
          format: date-time
          description: 错误发生时间
      required:
        - error
        - message

tags:
  - name: 用户管理
    description: 管理员用户管理相关接口
"#,
        // YAML containing the `function` keyword.
        r#"functions:
  - name: handler
    runtime: nodejs
    handler: index.handler"#,
        // YAML containing the `class` keyword.
        r#"classes:
  - name: Student
    properties:
      - id
      - name"#,
        // Docker Compose-style YAML.
        r#"version: '3'
services:
  web:
    image: nginx
    ports:
      - "80:80"
  db:
    image: postgres"#,
    ];

    for (i, yaml) in yaml_cases.iter().enumerate() {
        let result = TextDetector::detect(yaml);
        println!("\n=== Test case {} ===", i + 1);
        println!("YAML:\n{}", yaml);
        println!("Detection result: {:?}", result);
        println!("is_yaml: {}", TextDetector::is_yaml(yaml));
        println!(
            "has_code_indicators: {}",
            TextDetector::has_code_indicators(yaml)
        );
        if let ClipboardTextSyntax::Code { language } = &result {
            println!("Misclassified as code with language: {:?}", language);
        }

        assert!(
            matches!(result, ClipboardTextSyntax::Yaml),
            "case {} should be detected as YAML, but was detected as {:?}",
            i + 1,
            result
        );
    }
}
